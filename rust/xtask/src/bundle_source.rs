//! Explicit source-only formats. Release tar readers never dispatch through here.
use super::{Entry, ROOT, SOURCE, pair, sha};
use crate::package::archive::{DecodedArchive, cpio, decode_root};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

#[derive(Clone, Copy, PartialEq)]
pub(super) enum Format {
    BinaryCpio,
    IndexedBinaryCpio,
    Ustar,
}
impl Format {
    fn parse(value: &str) -> Result<Self, String> {
        match value {
            "binary-cpio" => Ok(Self::BinaryCpio),
            "indexed-binary-cpio" => Ok(Self::IndexedBinaryCpio),
            "ustar" => Ok(Self::Ustar),
            _ => Err("Source format must be indexed-binary-cpio, binary-cpio or ustar".into()),
        }
    }
    fn name(self) -> &'static str {
        match self {
            Self::BinaryCpio => "gzip-binary-cpio-be",
            Self::IndexedBinaryCpio => "gzip-indexed-binary-cpio-be",
            Self::Ustar => "gzip-ustar",
        }
    }
}
fn logical(files: &BTreeMap<String, Entry>) -> Result<usize, String> {
    files.values().try_fold(0usize, |sum, entry| {
        sum.checked_add(entry.bytes.len())
            .filter(|sum| *sum <= SOURCE.expanded)
            .ok_or_else(|| "Complete source logical bytes exceed32MiB".into())
    })
}
pub(super) fn decode(bytes: &[u8], format: Format) -> Result<DecodedArchive, String> {
    let mut decoded = match format {
        Format::BinaryCpio | Format::IndexedBinaryCpio => {
            cpio::binary_decode_root(bytes, ROOT, SOURCE, pair::source_limit)?
        }
        Format::Ustar => decode_root(bytes, ROOT, SOURCE)?,
    };
    logical(&decoded.files)?;
    if format == Format::IndexedBinaryCpio {
        pair::materialize_source(&mut decoded.files, SOURCE.expanded)?;
    }
    pair::source(&decoded.files, 4)?;
    logical(&decoded.files)?;
    if format != Format::Ustar {
        let manifest = &decoded.files["source-manifest.json"].bytes;
        let record = pair::metadata::parse(manifest, 1024 * 1024)?;
        if pair::canonical_record(&record, manifest.len())? != *manifest {
            return Err("Compact source JSON differs from its original canonical bytes".into());
        }
    }
    Ok(decoded)
}
fn report(
    mut value: Value,
    format: Format,
    files: &BTreeMap<String, Entry>,
) -> Result<Value, String> {
    value["schema_version"] = json!(1);
    value["kind"] = json!(match format {
        Format::BinaryCpio => "native_benchmark_compact_source_container",
        Format::IndexedBinaryCpio => "native_benchmark_indexed_source_container",
        Format::Ustar => "native_benchmark_legacy_source_inspection",
    });
    if format == Format::IndexedBinaryCpio {
        value["stored_manifest_schema_version"] = json!(4);
    }
    value["format"] = json!(format.name());
    value["source_manifest_schema_version"] = json!(2);
    value["logical_file_bytes"] = json!(logical(files)?);
    value["compressed_limit"] = json!(SOURCE.compressed);
    value["expanded_carrier_limit"] = json!(SOURCE.expanded);
    value["logical_file_limit"] = json!(SOURCE.expanded);
    Ok(value)
}
pub(super) fn create(root: &Path, destination: &Path, format: Format) -> Result<Value, String> {
    if format == Format::Ustar {
        return super::create_ustar(root, destination);
    }
    let sidecar = destination.with_file_name(format!(
        "{}.sha256",
        destination
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or("Invalid source archive filename")?
    ));
    if fs::symlink_metadata(destination).is_ok() || fs::symlink_metadata(sidecar).is_ok() {
        return Err("Refusing to overwrite an existing bundle or checksum".into());
    }
    let mut files = super::collect(root)?;
    logical(&files)?;
    let canonical = if format == Format::IndexedBinaryCpio {
        Some(pair::index_source(&mut files)?)
    } else {
        None
    };
    let bytes = cpio::binary_encode_root(ROOT, &files, SOURCE, pair::source_limit)?;
    let decoded = decode(&bytes, format)?;
    if decoded.files.len() != files.len()
        || files.iter().any(|(path, entry)| {
            let (name, expected) = match canonical.as_ref() {
                Some(original) if path == "source-manifest.bin" => {
                    ("source-manifest.json", original)
                }
                _ => (path.as_str(), entry),
            };
            decoded
                .files
                .get(name)
                .is_none_or(|actual| actual.bytes != expected.bytes || actual.mode != expected.mode)
        })
    {
        return Err("Compact source readback differs from original files".into());
    }
    report(
        super::publish_archive(
            destination,
            &bytes,
            files.len(),
            decoded.expanded_bytes,
            "expanded_carrier_bytes",
        )?,
        format,
        &decoded.files,
    )
}
struct Input {
    owner: pair::disk::Directory,
    decoded: DecodedArchive,
    bytes: usize,
    sha256: String,
}
fn read(path: &Path, format: Format) -> Result<Input, String> {
    let owner = pair::disk::Directory::open(path.parent().ok_or("Missing source archive parent")?)?;
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or("Invalid source archive filename")?;
    let bytes = owner.read(name, SOURCE.compressed)?;
    let checksum = sha(&bytes);
    if owner.read(&format!("{name}.sha256"), 1024)? != format!("{checksum}  {name}\n").as_bytes() {
        return Err("Source archive checksum sidecar differs".into());
    }
    let decoded = decode(&bytes, format)?;
    owner.attached()?;
    Ok(Input {
        owner,
        decoded,
        bytes: bytes.len(),
        sha256: checksum,
    })
}
fn inspect(path: &Path, format: Format, destination: Option<&Path>) -> Result<Value, String> {
    let input = read(path, format)?;
    if let Some(destination) = destination {
        if !destination.is_absolute() {
            return Err("Source destination must be a new absolute directory".into());
        }
        let files = super::file_bytes(&input.decoded.files);
        let output = pair::disk::create_tree(destination, &files, || Ok(()))?;
        input.owner.attached()?;
        output.attached()?;
        pair::disk::check_tree(&output, &files)?;
    }
    input.owner.attached()?;
    report(
        json!({"archive":path.to_string_lossy(),"sha256":input.sha256,
            "files":input.decoded.files.len(),"compressed_bytes":input.bytes,
            "expanded_carrier_bytes":input.decoded.expanded_bytes,
            "extracted_to":destination.map(|p|p.to_string_lossy()),
            "source_bytes_modes_verified":true,"provider_calls":0,
            "source_only":true,"requires_node":false,"dependencies_bundled":false,"release_approved":false}),
        format,
        &input.decoded.files,
    )
}
pub(super) fn run(args: &[String], root: &Path) -> Result<bool, String> {
    match args.first().map(String::as_str) {
        Some("--source-format") => {
            if args.len() != 3 || args[2].starts_with("--") {
                return Err(
                    "Usage: benchmark-bundle --source-format indexed-binary-cpio|binary-cpio|ustar NEW_ARCHIVE".into(),
                );
            }
            println!(
                "{}",
                serde_json::to_string_pretty(&create(
                    root,
                    &root.join(&args[2]),
                    Format::parse(&args[1])?
                )?)
                .map_err(|_| "Cannot encode source report")?
            );
        }
        Some("--inspect-source" | "--extract-source") => {
            let extracting = args[0] == "--extract-source";
            let expected = if extracting { 4 } else { 3 };
            if args.len() != expected {
                return Err("Usage: benchmark-bundle --inspect-source FORMAT ARCHIVE or --extract-source FORMAT ARCHIVE NEW_ABSOLUTE_DIRECTORY".into());
            }
            let destination = if extracting {
                Some(Path::new(&args[3]))
            } else {
                None
            };
            println!(
                "{}",
                serde_json::to_string_pretty(&inspect(
                    &root.join(&args[2]),
                    Format::parse(&args[1])?,
                    destination
                )?)
                .map_err(|_| "Cannot encode source inspection")?
            );
        }
        _ => return Ok(false),
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::package::archive::{ArchivePolicy, encode_root};
    use crate::tool_process::Scratch;
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    // A small complete synthetic source inventory proves legacy decoding, not
    // full-current-tree tar capacity. Full-tree assertions remain in bundle.rs.
    fn fixture() -> BTreeMap<String, Entry> {
        let mut files: BTreeMap<String, Entry> = BTreeMap::from([
            (
                "test/fixtures/ollama-routing.json".into(),
                super::super::data_entry(b"{}".to_vec()),
            ),
            (
                "README.native.md".into(),
                super::super::data_entry(b"synthetic source codec fixture\n".to_vec()),
            ),
        ]);
        let hashes = files
            .iter()
            .map(|(p, e)| (p.clone(), sha(&e.bytes)))
            .collect();
        files.insert(
            "source-manifest.json".into(),
            super::super::json_entry(&super::super::source_record(&hashes), "fixture").unwrap(),
        );
        files
    }
    fn same(actual: &BTreeMap<String, Entry>, expected: &BTreeMap<String, Entry>) {
        assert_eq!(actual.len(), expected.len());
        for (path, entry) in expected {
            assert_eq!(actual[path].bytes, entry.bytes, "{path}");
            assert_eq!(actual[path].mode, entry.mode, "{path}");
        }
    }
    fn store(directory: &Path, name: &str, bytes: &[u8]) -> std::path::PathBuf {
        let path = directory.join(name);
        fs::write(&path, bytes).unwrap();
        fs::write(
            directory.join(format!("{name}.sha256")),
            format!("{}  {name}\n", sha(bytes)),
        )
        .unwrap();
        path
    }
    #[test]
    fn compact_full_tree_preserves_original_generated_files_and_independent_checkout_bytes() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .canonicalize()
            .unwrap();
        let expected = super::super::collect(&root).unwrap();
        let scratch = Scratch::new("compact-full-tree").unwrap();
        let archive = scratch.0.join("source.cpio.gz");
        let result = super::super::create(&root, &archive).unwrap();
        let bytes = fs::read(&archive).unwrap();
        let decoded = decode(&bytes, Format::IndexedBinaryCpio).unwrap();
        same(&decoded.files, &expected);
        assert_eq!(result["logical_file_bytes"], logical(&expected).unwrap());
        assert_eq!(result["expanded_carrier_bytes"], decoded.expanded_bytes);
        assert!(result.get("expanded_tar_bytes").is_none());
        assert_eq!(
            decoded
                .files
                .keys()
                .filter(|p| p.starts_with("rust/parity/cases/"))
                .count(),
            89
        );
        assert_eq!(
            decoded
                .files
                .keys()
                .filter(|p| p.starts_with("rust/vendor/"))
                .count(),
            265 // Includes both explicitly allowed OpenSSL initialization/default modules.
        );
        assert!(
            decoded
                .files
                .contains_key("rust/vendor/openssl/src/node_init.rs")
        );
        assert!(
            decoded
                .files
                .contains_key("rust/vendor/openssl/src/ssl/node_defaults.rs")
        );
        assert!(
            decoded
                .files
                .contains_key("rust/supplier-tests/openssl-native/Cargo.toml")
        );
        assert!(
            decoded
                .files
                .contains_key("rust/supplier-tests/openssl-native/Cargo.lock")
        );
        for (path, entry) in &decoded.files {
            if !["README.native.md", "source-manifest.json"].contains(&path.as_str()) {
                assert_eq!(entry.bytes, fs::read(root.join(path)).unwrap(), "{path}");
            }
        }
        let output = scratch.0.join("complete restored source");
        inspect(&archive, Format::IndexedBinaryCpio, Some(&output)).unwrap();
        for (path, entry) in &expected {
            assert_eq!(fs::read(output.join(path)).unwrap(), entry.bytes, "{path}");
            assert_eq!(
                fs::metadata(output.join(path))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o7777,
                entry.mode
            );
        }
        let owner = pair::disk::Directory::open(&output).unwrap();
        pair::disk::check_tree(&owner, &super::super::file_bytes(&expected)).unwrap();
        assert!(super::super::create(&root, &archive).is_err());
        assert_eq!(fs::read(&archive).unwrap(), bytes);
    }
    #[test]
    fn explicit_source_codecs_preserve_legacy_bytes_and_never_fallback() {
        let files = fixture();
        let tar = encode_root(ROOT, &files, SOURCE).unwrap();
        let compact = cpio::binary_encode_root(ROOT, &files, SOURCE, pair::source_limit).unwrap();
        same(&decode(&tar, Format::Ustar).unwrap().files, &files);
        same(&decode(&compact, Format::BinaryCpio).unwrap().files, &files);
        assert!(decode(&tar, Format::BinaryCpio).is_err());
        assert!(decode(&compact, Format::Ustar).is_err());
        for (bytes, format) in [(&tar, Format::Ustar), (&compact, Format::BinaryCpio)] {
            assert!(decode(&bytes[..bytes.len() - 1], format).is_err());
        }
        let expanded = decode_root(&tar, ROOT, SOURCE).unwrap().expanded_bytes;
        assert!(decode_root(&tar, ROOT, SOURCE.remaining(expanded - 1)).is_err());
        let expanded = cpio::binary_decode_root(&compact, ROOT, SOURCE, pair::source_limit)
            .unwrap()
            .expanded_bytes;
        assert!(
            cpio::binary_decode_root(
                &compact,
                ROOT,
                SOURCE.remaining(expanded - 1),
                pair::source_limit
            )
            .is_err()
        );
        assert!(Format::parse("tar-or-cpio").is_err());
        let old = SOURCE;
        assert_eq!(
            (old.compressed, old.expanded, old.file, old.entries),
            (33554432, 33554432, 33554432, 65534)
        );
        // Same SOURCE policy remains the argument to both codec admissions.
        assert!(
            decode_root(
                &tar,
                ROOT,
                ArchivePolicy {
                    compressed: tar.len() - 1,
                    ..SOURCE
                }
            )
            .is_err()
        );
        assert!(
            cpio::binary_decode_root(
                &compact,
                ROOT,
                ArchivePolicy {
                    compressed: compact.len() - 1,
                    ..SOURCE
                },
                pair::source_limit
            )
            .is_err()
        );
    }
    #[test]
    fn compact_source_admission_rejects_changed_headers_inventory_modes_and_bytes() {
        let original = fixture();
        for mutation in 0..5 {
            let mut files = original.clone();
            match mutation {
                0 => files
                    .get_mut("source-manifest.json")
                    .unwrap()
                    .bytes
                    .push(b'\n'),
                1 => {
                    let mut value =
                        pair::metadata::parse(&files["source-manifest.json"].bytes, 1024 * 1024)
                            .unwrap();
                    value["build_requires"] =
                        json!("Different instructions must not be normalized away");
                    files.get_mut("source-manifest.json").unwrap().bytes =
                        serde_json::to_vec_pretty(&value).unwrap();
                }
                2 => files.get_mut("README.native.md").unwrap().bytes.push(b'x'),
                3 => {
                    files.insert(
                        "config.json".into(),
                        super::super::data_entry(b"{}".to_vec()),
                    );
                }
                _ => {
                    files.remove("README.native.md");
                }
            }
            let bytes = cpio::binary_encode_root(ROOT, &files, SOURCE, pair::source_limit).unwrap();
            assert!(
                decode(&bytes, Format::BinaryCpio).is_err(),
                "mutation{mutation}"
            );
        }
        let mut files = original;
        files.get_mut("README.native.md").unwrap().mode = 0o755;
        assert!(cpio::binary_encode_root(ROOT, &files, SOURCE, pair::source_limit).is_err());
    }
    fn inspection_contract(format: Format, format_name: &str, kind: &str) {
        let scratch = Scratch::new("compact-source-restore").unwrap();
        let files = fixture();
        let mut stored = files.clone();
        if format == Format::IndexedBinaryCpio {
            pair::index_source(&mut stored).unwrap();
        }
        let bytes = cpio::binary_encode_root(ROOT, &stored, SOURCE, pair::source_limit).unwrap();
        let path = store(&scratch.0, "source.cpio.gz", &bytes);
        let report = inspect(&path, format, None).unwrap();
        assert_eq!(report["files"], files.len());
        assert_eq!(report["sha256"], sha(&bytes));
        assert_eq!(report["format"], format_name);
        assert_eq!(report["kind"], kind);
        assert_eq!(fs::read_dir(&scratch.0).unwrap().count(), 2);
        let destination = scratch.0.join("restored source with spaces");
        let restored = inspect(&path, format, Some(&destination)).unwrap();
        assert_eq!(restored["extracted_to"], destination.to_str().unwrap());
        let owner = pair::disk::Directory::open(&destination).unwrap();
        pair::disk::check_tree(&owner, &super::super::file_bytes(&files)).unwrap();
        assert_eq!(
            fs::metadata(&destination).unwrap().permissions().mode() & 0o7777,
            0o700
        );
        for (name, entry) in &files {
            let actual = fs::symlink_metadata(destination.join(name)).unwrap();
            assert!(actual.is_file() && !actual.file_type().is_symlink());
            assert_eq!(actual.nlink(), 1);
            assert_eq!(actual.permissions().mode() & 0o7777, 0o644);
            assert_eq!(fs::read(destination.join(name)).unwrap(), entry.bytes);
        }
        assert!(inspect(&path, format, Some(&destination)).is_err());
        assert!(inspect(&path, format, Some(Path::new("relative"))).is_err());
        let sidecar = scratch.0.join("source.cpio.gz.sha256");
        fs::write(&sidecar, "invalid checksum\n").unwrap();
        let rejected = scratch.0.join("must not be created");
        assert!(inspect(&path, format, Some(&rejected)).is_err());
        assert!(!rejected.exists());
        fs::write(&sidecar, format!("{}  source.cpio.gz\n", sha(&bytes))).unwrap();
        let link = scratch.0.join("linked.cpio.gz");
        std::os::unix::fs::symlink(&path, &link).unwrap();
        assert!(inspect(&link, format, Some(&rejected)).is_err());
        assert!(!rejected.exists());
        let hardlink = scratch.0.join("hardlinked archive");
        fs::hard_link(&path, &hardlink).unwrap();
        assert!(inspect(&path, format, Some(&rejected)).is_err());
        assert!(!rejected.exists());
        fs::remove_file(&hardlink).unwrap();
        let link_dest = scratch.0.join("linked destination");
        std::os::unix::fs::symlink(&destination, &link_dest).unwrap();
        assert!(inspect(&path, format, Some(&link_dest)).is_err());
        pair::disk::check_tree(&owner, &super::super::file_bytes(&files)).unwrap();
    }
    #[test]
    fn compact_source_inspection_and_owned_restore_retain_every_byte_mode_and_refuse_overwrite() {
        inspection_contract(
            Format::BinaryCpio,
            "gzip-binary-cpio-be",
            "native_benchmark_compact_source_container",
        );
    }
    #[test]
    fn indexed_source_inspection_and_restore_apply_all_existing_descriptor_controls() {
        inspection_contract(
            Format::IndexedBinaryCpio,
            "gzip-indexed-binary-cpio-be",
            "native_benchmark_indexed_source_container",
        );
    }
    #[test]
    fn indexed_source_reconstructs_original_json_and_rejects_foreign_or_changed_inventory() {
        let expected = fixture();
        let mut stored = expected.clone();
        let original = pair::index_source(&mut stored).unwrap();
        assert_eq!(original.bytes, expected["source-manifest.json"].bytes);
        let encode = |files: &BTreeMap<String, Entry>| {
            cpio::binary_encode_root(ROOT, files, SOURCE, pair::source_limit).unwrap()
        };
        let bytes = encode(&stored);
        same(
            &decode(&bytes, Format::IndexedBinaryCpio).unwrap().files,
            &expected,
        );
        assert!(decode(&bytes, Format::BinaryCpio).is_err());
        assert!(decode(&bytes, Format::Ustar).is_err());
        assert!(decode(&encode(&expected), Format::IndexedBinaryCpio).is_err());
        let mut tight = stored.clone();
        assert!(pair::materialize_source(&mut tight, logical(&expected).unwrap() - 1).is_err());
        assert!(
            tight.contains_key("source-manifest.bin")
                && !tight.contains_key("source-manifest.json")
        );
        pair::materialize_source(&mut tight, logical(&expected).unwrap()).unwrap();
        same(&tight, &expected);
        for mutation in 0..5 {
            let mut bad = stored.clone();
            match mutation {
                0 => {
                    bad.get_mut("source-manifest.bin").unwrap().bytes.pop();
                }
                1 => bad.get_mut("README.native.md").unwrap().bytes.push(b'x'),
                2 => {
                    bad.insert("source-manifest.json".into(), original.clone());
                }
                3 => {
                    bad.remove("README.native.md");
                }
                _ => {
                    let mut record = pair::metadata::parse(&original.bytes, 1024 * 1024).unwrap();
                    record["build_requires"] =
                        json!("Different instructions must not be normalized away");
                    bad.get_mut("source-manifest.bin").unwrap().bytes =
                        pair::metadata::encode_source(&record, 1024 * 1024).unwrap();
                }
            }
            assert!(
                decode(&encode(&bad), Format::IndexedBinaryCpio).is_err(),
                "mutation{mutation}"
            );
        }
        assert!(Format::parse("indexed-binary-cpio").unwrap() == Format::IndexedBinaryCpio);
    }
}
