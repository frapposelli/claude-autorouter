//! Synthetic format admission; original complete writer coverage stays in transfer tests.
use super::*;
use crate::tool_process::Scratch;
use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::PathBuf;

#[test]
fn binary_source_manifest_literal_rows_are_lossless_bounded_and_strict() {
    // Independently authored bytes: SHA256 of UTF8 ["a","ab"], followed
    // by two full digest rows. No product encoder constructs this oracle.
    let header = br#"{"schema_version":4,"kind":"native_benchmark_public_source_bundle","fixture_sha256":"0000000000000000000000000000000000000000000000000000000000000000","build_requires":"synthetic","private_material_included":false,"live_calls_performed":false,"files_encoding":"indexed-native-paths-sha256-v1","paths_sha256":"d052ce3ea04741c5c4621d37e1df1ad56afa773955822ff3795cf4fa818d10f6"}"#;
    let paths = ["a", "ab"];
    let rows = [&[0; 32][..], &[0x11; 32]].concat();
    let packet = |header: &[u8], rows: &[u8]| {
        let mut bytes = b"ARSM\0\0\0\x04".to_vec();
        bytes.extend_from_slice(&(header.len() as u32).to_be_bytes());
        bytes.extend_from_slice(header);
        bytes.extend_from_slice(&2u32.to_be_bytes());
        bytes.extend_from_slice(rows);
        bytes
    };
    let bytes = packet(header, &rows);
    let decoded = metadata::decode_source(&bytes, FILE_LIMIT, &paths).unwrap();
    assert_eq!(
        decoded["files"],
        json!({"a":"00".repeat(32),"ab":"11".repeat(32)})
    );
    assert_eq!(decoded["schema_version"], 2);
    let encoded = metadata::encode_source(&decoded, FILE_LIMIT).unwrap();
    assert_eq!(&encoded[encoded.len() - rows.len()..], &rows);
    assert_eq!(
        metadata::decode_source(&encoded, FILE_LIMIT, &paths).unwrap(),
        decoded
    );
    let mut unicode = decoded.clone();
    let unicode_paths = ["src/café\"a.rs", "src/cafê-b.rs"];
    unicode["files"] = json!({unicode_paths[0]:"00".repeat(32),unicode_paths[1]:"11".repeat(32)});
    let encoded_unicode = metadata::encode_source(&unicode, FILE_LIMIT).unwrap();
    assert_eq!(
        metadata::decode_source(&encoded_unicode, FILE_LIMIT, &unicode_paths).unwrap(),
        unicode
    );
    let header_end = 12 + u32::from_be_bytes(encoded_unicode[8..12].try_into().unwrap()) as usize;
    assert_eq!(
        metadata::parse(&encoded_unicode[12..header_end], FILE_LIMIT).unwrap()["paths_sha256"],
        "f3b350b3ce120e7a9cc7bc666cd6c78129c25c3b76947d2f0e10856c1f48b604"
    );
    for end in 0..bytes.len() {
        assert!(metadata::decode_source(&bytes[..end], FILE_LIMIT, &paths).is_err());
    }
    assert!(metadata::decode_source(&bytes, bytes.len() - 1, &paths).is_err());
    assert!(metadata::decode_source(&bytes, FILE_LIMIT, &["a", "ab", "c"]).is_err());
    assert!(metadata::encode_source(&decoded, encoded.len() - 1).is_err());
    let mut bad = bytes.clone();
    bad.push(0);
    assert!(metadata::decode_source(&bad, FILE_LIMIT, &paths).is_err());
    let mut bad = bytes.clone();
    bad[7] = 3;
    assert!(metadata::decode_source(&bad, FILE_LIMIT, &paths).is_err());
    let mut bad = bytes.clone();
    bad[8..12].copy_from_slice(&u32::MAX.to_be_bytes());
    assert!(metadata::decode_source(&bad, FILE_LIMIT, &paths).is_err());
    for count in [0u32, 1, 3, u32::MAX] {
        let mut bad = bytes.clone();
        bad[12 + header.len()..16 + header.len()].copy_from_slice(&count.to_be_bytes());
        assert!(metadata::decode_source(&bad, FILE_LIMIT, &paths).is_err());
    }
    let text = std::str::from_utf8(header).unwrap();
    for altered in [
        text.replace("sha256-v1", "sha256-v2"),
        text.replace(
            "indexed-native-paths-sha256-v1",
            "sorted-utf8-prefix-sha256-binary-v1",
        ),
        text.replace("schema_version\":4", "schema_version\":3"),
        text.replacen('{', "{\"schema_version\":4,", 1),
        text.replacen('{', "{\"foreign\":0,", 1),
        text.replace("d052ce3e", "D052ce3e"),
        text.replace("paths_sha256", "missing_sha256"),
    ] {
        assert!(
            metadata::decode_source(&packet(altered.as_bytes(), &rows), FILE_LIMIT, &paths)
                .is_err()
        );
    }
    for invalid in [
        ["ab", "a"],
        ["a", "a"],
        ["", "ab"],
        ["../a", "ab"],
        ["/a", "ab"],
        ["a\\b", "ab"],
        ["a\n", "ab"],
    ] {
        assert!(
            metadata::decode_source(&bytes, FILE_LIMIT, &invalid)
                .unwrap_err()
                .contains("Unsafe")
        );
    }
    // Same count and sort position are insufficient: the committed names must match.
    assert!(
        metadata::decode_source(&bytes, FILE_LIMIT, &["a", "ac"])
            .unwrap_err()
            .contains("commitment")
    );
    let long = "a".repeat(257);
    assert!(
        metadata::decode_source(&bytes, FILE_LIMIT, &[&long, "b"])
            .unwrap_err()
            .contains("Unsafe")
    );
    let mut large = decoded.clone();
    let long_paths = ["a".repeat(256), "b".repeat(256)];
    large["files"] =
        json!({long_paths[0].clone():"00".repeat(32),long_paths[1].clone():"11".repeat(32)});
    let encoded_large = metadata::encode_source(&large, FILE_LIMIT).unwrap();
    assert!(
        metadata::decode_source(
            &encoded_large,
            encoded_large.len(),
            &[&long_paths[0], &long_paths[1]]
        )
        .unwrap_err()
        .contains("Decoded")
    );
    let mut bad_hash = decoded.clone();
    bad_hash["files"]["a"] = json!("GG".repeat(32));
    assert!(metadata::encode_source(&bad_hash, FILE_LIMIT).is_err());
    let mut huge = decoded.clone();
    huge["build_requires"] = json!("x".repeat(64 * 1024));
    assert!(metadata::encode_source(&huge, FILE_LIMIT).is_err());
}

#[test]
fn schema5_binary_facts_preserve_payloads_and_reject_version_or_charge_downgrades() {
    let f = Fixture::new();
    for variant in [
        "good",
        "old-json",
        "downgrade",
        "both",
        "changed-hash",
        "swapped-hashes",
        "charge",
    ] {
        let directory = f.packet(variant, true, |_, _, _| {});
        f.schema4(&directory, |native, reference, record| {
            record["schema_version"] = json!(if variant == "downgrade" { 4 } else { 5 });
            if variant != "old-json" {
                let original = native.remove("source-manifest.json").unwrap();
                let facts = metadata::parse(&original.bytes, FILE_LIMIT).unwrap();
                let mut bytes = metadata::encode_source(&facts, FILE_LIMIT).unwrap();
                if variant == "changed-hash" {
                    *bytes.last_mut().unwrap() ^= 1;
                }
                if variant == "swapped-hashes" {
                    let at = bytes.len() - 64;
                    let (first, second) = bytes[at..].split_at_mut(32);
                    first.swap_with_slice(second);
                }
                native.insert(
                    "source-manifest.bin".into(),
                    Entry {
                        bytes,
                        mode: original.mode,
                    },
                );
                if variant == "both" {
                    native.insert("source-manifest.json".into(), original);
                }
            }
            let payload = sum(native
                .values()
                .map(|e| e.bytes.as_slice())
                .chain(f.reference.values().map(|e| e.bytes.as_slice())))
            .unwrap();
            record["aggregate_logical_file_bytes"] =
                json!(payload - usize::from(variant == "charge"));
            let legacy = legacy_full(native, expanded_size(reference, SOURCE).unwrap(), 5).unwrap();
            record["legacy_full_tar_bytes"] = json!(legacy);
            record["legacy_full_tar_within_limit"] = json!(legacy <= SOURCE.expanded);
        });
        if variant != "good" {
            f.reject(&directory, &format!("{variant}-output"));
            continue;
        }
        let plan = load(&f.root, &directory).unwrap();
        assert_eq!(plan.schema, 5);
        assert_eq!(plan.record["aggregate_expanded_limit"], SOURCE.expanded);
        assert_eq!(plan.record["aggregate_logical_file_limit"], SOURCE.expanded);
        let facts = metadata::decode_source(
            &plan.native.files["source-manifest.bin"].bytes,
            FILE_LIMIT,
            &plan
                .native
                .files
                .keys()
                .filter(|path| path.as_str() != "source-manifest.bin")
                .map(String::as_str)
                .collect::<Vec<_>>(),
        )
        .unwrap();
        assert_eq!(
            facts,
            metadata::parse(&f.native["source-manifest.json"].bytes, FILE_LIMIT).unwrap()
        );
        for (path, entry) in &f.native {
            if path != "source-manifest.json" {
                assert_eq!(plan.native.files[path].bytes, entry.bytes);
                assert_eq!(plan.native.files[path].mode, entry.mode);
            }
        }
        let output = f.scratch.0.join("binary-restored");
        assert_eq!(
            restore(&f.root, &directory, &output).unwrap()["logical_files"],
            113
        );
        for (path, entry) in &f.reference {
            assert_eq!(fs::read(output.join(path)).unwrap(), entry.bytes);
            assert_eq!(
                fs::metadata(output.join(path))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                entry.mode
            );
        }
    }
}

struct Fixture {
    scratch: Scratch,
    root: PathBuf,
    native: BTreeMap<String, Entry>,
    reference: BTreeMap<String, Entry>,
}
impl Fixture {
    fn new() -> Self {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .canonicalize()
            .unwrap();
        let (reference, baseline) = reference(&root).unwrap();
        let mut native = BTreeMap::from([
            (
                "rust/parity/baseline.json".into(),
                Entry {
                    bytes: baseline,
                    mode: 0o644,
                },
            ),
            ("LICENSE".into(), reference["LICENSE"].clone()),
        ]);
        for (path, _, _) in SHARED {
            native.insert((*path).into(), reference[*path].clone());
        }
        Self::manifest(&mut native);
        Self {
            scratch: Scratch::new("paired-format").unwrap(),
            root,
            native,
            reference,
        }
    }
    fn manifest(native: &mut BTreeMap<String, Entry>) {
        let hashes: BTreeMap<_, _> = native
            .iter()
            .filter(|(path, _)| path.as_str() != "source-manifest.json")
            .map(|(path, entry)| (path.clone(), sha(&entry.bytes)))
            .collect();
        let record = json!({"schema_version":2,"kind":"native_benchmark_public_source_bundle",
            "fixture_sha256":hashes["test/fixtures/ollama-routing.json"],"files":hashes,
            "build_requires":"Pinned Rust toolchain and locked dependencies","private_material_included":false,"live_calls_performed":false});
        native.insert(
            "source-manifest.json".into(),
            Entry {
                bytes: serde_json::to_vec(&record).unwrap(),
                mode: 0o644,
            },
        );
    }
    /// Historical subset is intentional: schema1 receiver honors carried manifest,
    /// rather than imposing today's larger FIXED inventory on old source bundles.
    fn packet(
        &self,
        name: &str,
        reused: bool,
        mutate: impl FnOnce(&mut BTreeMap<String, Entry>, &mut BTreeMap<String, Entry>, &mut Value),
    ) -> PathBuf {
        let directory = self.scratch.0.join(name);
        fs::create_dir(&directory).unwrap();
        let mut native = self.native.clone();
        let mut reference = self.reference.clone();
        if reused {
            for (path, _, _) in SHARED {
                reference.remove(*path).unwrap();
            }
        }
        let baseline: Value =
            serde_json::from_slice(&self.reference["frozen-reference-manifest.json"].bytes)
                .unwrap();
        let mut record = json!({"schema_version":if reused{2}else{1},"kind":"matched_native_benchmark_transfer",
            "baseline_commit":baseline["baseline_commit"],"baseline_version":baseline["baseline_version"],"reference_files":111,
            "archives":{},"aggregate_expanded_tar_bytes":0,"aggregate_expanded_limit":SOURCE.expanded,
            "node_runtime_required":"22.14.0 for temporary reference tests only","native_build_required":true,
            "private_material_included":false,"binaries_included":false,"dependencies_bundled":false,"provider_calls":0,
            "release_approved":false,"performance_accepted":false});
        if reused {
            let payload = sum(native
                .values()
                .map(|entry| entry.bytes.as_slice())
                .chain(self.reference.values().map(|entry| entry.bytes.as_slice())))
            .unwrap();
            for (key, value) in [
                ("logical_reference_files", json!(113)),
                ("reference_transport_files", json!(111)),
                (
                    "shared_inputs",
                    shared(2, &self.reference["frozen-reference-manifest.json"].bytes),
                ),
                ("aggregate_logical_file_bytes", json!(payload)),
                ("aggregate_logical_file_limit", json!(SOURCE.expanded)),
            ] {
                record[key] = value;
            }
        }
        mutate(&mut native, &mut reference, &mut record);
        let archive_name = if reused {
            DELTA_NAME
        } else {
            "frozen-node-reference.tar.gz"
        };
        let first = encode_root(NATIVE_ROOT, &native, SOURCE).unwrap();
        let native_size = expanded_size(&native, SOURCE).unwrap();
        let policy = SOURCE.remaining(SOURCE.expanded - native_size);
        let second = encode_root(
            if reused {
                DELTA_ROOT
            } else {
                "autorouter-node-reference"
            },
            &reference,
            policy,
        )
        .unwrap();
        let stored = native_size + expanded_size(&reference, policy).unwrap();
        record["archives"] = json!({"native-source.tar.gz":{"sha256":sha(&first),"expanded_tar_bytes":native_size},
            (archive_name):{"sha256":sha(&second),"expanded_tar_bytes":stored-native_size}});
        record["aggregate_expanded_tar_bytes"] = json!(stored);
        if reused {
            record["legacy_full_tar_bytes"] = json!(stored + 79360);
            record["legacy_full_tar_within_limit"] = json!(stored + 79360 <= SOURCE.expanded);
        }
        fs::write(directory.join("native-source.tar.gz"), first).unwrap();
        fs::write(directory.join(archive_name), second).unwrap();
        Self::metadata(&directory, &record, archive_name);
        directory
    }
    fn metadata(directory: &Path, record: &Value, name: &str) {
        let raw = serde_json::to_vec(record).unwrap();
        Self::raw_metadata(directory, &raw, name);
    }
    fn raw_metadata(directory: &Path, raw: &[u8], name: &str) {
        fs::write(directory.join("transfer-manifest.json"), raw).unwrap();
        let resume = super::super::transfer::README.as_bytes();
        fs::write(directory.join("RESUME.md"), resume).unwrap();
        let first = format!(
            "{}  native-source.tar.gz\n",
            sha(&fs::read(directory.join("native-source.tar.gz")).unwrap())
        );
        let second = format!(
            "{}  {name}\n",
            sha(&fs::read(directory.join(name)).unwrap())
        );
        fs::write(directory.join("native-source.tar.gz.sha256"), &first).unwrap();
        fs::write(directory.join(format!("{name}.sha256")), &second).unwrap();
        fs::write(
            directory.join("SHA256SUMS"),
            format!(
                "{first}{second}{}  transfer-manifest.json\n{}  RESUME.md\n",
                sha(raw),
                sha(resume)
            ),
        )
        .unwrap();
    }
    fn schema3(&self, directory: &Path) {
        let mut record: Value =
            serde_json::from_slice(&fs::read(directory.join("transfer-manifest.json")).unwrap())
                .unwrap();
        let native = cpio::encode_root(NATIVE_ROOT, &self.native, SOURCE, source_limit).unwrap();
        let native_expanded =
            cpio::expanded_size(NATIVE_ROOT, &self.native, SOURCE, source_limit).unwrap();
        let delta_expanded = record["archives"][DELTA_NAME]["expanded_tar_bytes"]
            .as_u64()
            .unwrap() as usize;
        record["schema_version"] = json!(3);
        record
            .as_object_mut()
            .unwrap()
            .remove("aggregate_expanded_tar_bytes");
        record["aggregate_expanded_carrier_bytes"] = json!(native_expanded + delta_expanded);
        record["archives"] = json!({(CPIO_NAME):{"sha256":sha(&native),"expanded_carrier_bytes":native_expanded,"format":"gzip-newc"},
            (DELTA_NAME):{"sha256":sha(&fs::read(directory.join(DELTA_NAME)).unwrap()),"expanded_carrier_bytes":delta_expanded,"format":"gzip-ustar"}});
        // Legacy diagnostic comes from the old actual tarfixture, not a CPIO size.
        fs::remove_file(directory.join("native-source.tar.gz")).unwrap();
        fs::remove_file(directory.join("native-source.tar.gz.sha256")).unwrap();
        fs::write(directory.join(CPIO_NAME), native).unwrap();
        Self::v3_metadata(directory, &record);
    }
    fn schema4(
        &self,
        directory: &Path,
        mutate: impl FnOnce(&mut BTreeMap<String, Entry>, &mut BTreeMap<String, Entry>, &mut Value),
    ) {
        self.schema3(directory);
        let mut record: Value =
            serde_json::from_slice(&fs::read(directory.join("transfer-manifest.json")).unwrap())
                .unwrap();
        let mut native = self.native.clone();
        let mut reference = decode_root(
            &fs::read(directory.join(DELTA_NAME)).unwrap(),
            DELTA_ROOT,
            SOURCE,
        )
        .unwrap()
        .files;
        for path in ["LICENSE", "frozen-reference-manifest.json"] {
            reference.remove(path).unwrap();
        }
        record["schema_version"] = json!(4);
        record["reference_transport_files"] = json!(109);
        record["shared_inputs"] =
            shared(4, &self.reference["frozen-reference-manifest.json"].bytes);
        mutate(&mut native, &mut reference, &mut record);
        let first = cpio::encode_root(NATIVE_ROOT, &native, SOURCE, source_limit).unwrap();
        let first_size = cpio::expanded_size(NATIVE_ROOT, &native, SOURCE, source_limit).unwrap();
        let policy = SOURCE.remaining(SOURCE.expanded - first_size);
        let second = encode_root(DELTA_ROOT, &reference, policy).unwrap();
        let second_size = expanded_size(&reference, policy).unwrap();
        record["aggregate_expanded_carrier_bytes"] = json!(first_size + second_size);
        record["archives"] = json!({
            (CPIO_NAME):{"sha256":sha(&first),"expanded_carrier_bytes":first_size,"format":"gzip-newc"},
            (DELTA_NAME):{"sha256":sha(&second),"expanded_carrier_bytes":second_size,"format":"gzip-ustar"}});
        fs::write(directory.join(CPIO_NAME), first).unwrap();
        fs::write(directory.join(DELTA_NAME), second).unwrap();
        Self::v3_metadata(directory, &record);
    }
    fn v3_metadata(directory: &Path, record: &Value) {
        let bytes = serde_json::to_vec(record).unwrap();
        fs::write(directory.join("transfer-manifest.json"), &bytes).unwrap();
        let resume = fs::read(directory.join("RESUME.md")).unwrap();
        let native = format!(
            "{}  {CPIO_NAME}\n",
            sha(&fs::read(directory.join(CPIO_NAME)).unwrap())
        );
        let delta = format!(
            "{}  {DELTA_NAME}\n",
            sha(&fs::read(directory.join(DELTA_NAME)).unwrap())
        );
        fs::write(directory.join(format!("{CPIO_NAME}.sha256")), &native).unwrap();
        fs::write(directory.join(format!("{DELTA_NAME}.sha256")), &delta).unwrap();
        fs::write(
            directory.join("SHA256SUMS"),
            format!(
                "{native}{delta}{}  transfer-manifest.json\n{}  RESUME.md\n",
                sha(&bytes),
                sha(&resume)
            ),
        )
        .unwrap();
    }
    fn reject(&self, directory: &Path, name: &str) {
        let output = self.scratch.0.join(name);
        let error = restore(&self.root, directory, &output).unwrap_err();
        assert!(!error.contains("PRIVATE_SENTINEL"));
        assert!(
            !output.exists(),
            "invalid carrier must fail before destination creation"
        );
    }
}
#[test]
fn legacy_and_shared_carriers_restore_all_original_113_bytes_modes_and_license() {
    let f = Fixture::new();
    for reused in [false, true] {
        let name = if reused { "shared" } else { "legacy" };
        let directory = f.packet(name, reused, |_, _, _| {});
        let plan = load(&f.root, &directory).unwrap();
        assert_eq!(plan.reference.files.len(), if reused { 111 } else { 113 });
        assert!(plan.reference.files.contains_key("LICENSE"));
        for (path, _, _) in SHARED {
            assert_eq!(plan.reference.files.contains_key(*path), !reused);
        }
        let output = f.scratch.0.join(format!("{name}-restored"));
        let report = restore(&f.root, &directory, &output).unwrap();
        assert_eq!(report["logical_files"], 113);
        assert_eq!(report["reference_files"], 111);
        assert_eq!(report["provider_calls"], 0);
        assert_eq!(report["release_approved"], false);
        for (path, entry) in &f.reference {
            assert_eq!(fs::read(output.join(path)).unwrap(), entry.bytes);
            assert_eq!(
                fs::metadata(output.join(path))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                entry.mode
            );
        }
        let before = fs::read(output.join("LICENSE")).unwrap();
        assert!(restore(&f.root, &directory, &output).is_err());
        assert_eq!(fs::read(output.join("LICENSE")).unwrap(), before);
    }
}
#[test]
fn sharing_and_equal_count_missing_foreign_corruption_fail_before_writes() {
    let f = Fixture::new();
    let variants = [
        "donor_missing",
        "donor_changed",
        "donor_mode",
        "duplicate_physical",
        "same_count_foreign",
        "manifest_removed",
        "license_changed",
        "source_hash",
        "source_duplicate_key",
        "foreign_source",
    ];
    for variant in variants {
        let directory = f.packet(variant, true, |native, reference, _| match variant {
            "donor_missing" => {
                native.remove(SHARED[0].0).unwrap();
                Fixture::manifest(native);
            }
            "donor_changed" => {
                native.get_mut(SHARED[0].0).unwrap().bytes[0] ^= 1;
                Fixture::manifest(native);
            }
            "donor_mode" => native.get_mut(SHARED[0].0).unwrap().mode = 0o600,
            "duplicate_physical" => {
                reference.insert(SHARED[0].0.into(), native[SHARED[0].0].clone());
            }
            "same_count_foreign" => {
                let path = reference
                    .keys()
                    .find(|path| {
                        !["LICENSE", "frozen-reference-manifest.json"].contains(&path.as_str())
                    })
                    .unwrap()
                    .clone();
                let old = reference.remove(&path).unwrap();
                reference.insert("foreign.mjs".into(), old);
                assert_eq!(reference.len(), 111);
            }
            "manifest_removed" => {
                reference.remove("frozen-reference-manifest.json").unwrap();
            }
            "license_changed" => reference.get_mut("LICENSE").unwrap().bytes[0] ^= 1,
            "source_hash" => native.get_mut(SHARED[0].0).unwrap().bytes[0] ^= 1,
            "source_duplicate_key" => {
                let bytes = &mut native.get_mut("source-manifest.json").unwrap().bytes;
                *bytes = String::from_utf8(bytes.clone())
                    .unwrap()
                    .replacen('{', "{\"schema_version\":2,", 1)
                    .into_bytes();
            }
            "foreign_source" => {
                native.insert(
                    "private.env".into(),
                    Entry {
                        bytes: b"PRIVATE_SENTINEL".to_vec(),
                        mode: 0o644,
                    },
                );
                Fixture::manifest(native);
            }
            _ => unreachable!(),
        });
        f.reject(&directory, &format!("{variant}-output"));
    }
}
#[test]
fn exact_json_fields_duplicate_keys_shared_labels_and_limits_are_enforced() {
    let f = Fixture::new();
    for variant in [
        "foreign",
        "schema",
        "shared_duplicate",
        "shared_foreign",
        "shared_missing",
        "logical_limit",
        "logical_payload",
        "logical_count",
        "physical_count",
        "privacy",
        "provider",
        "missing_archive",
        "expansion",
        "legacy",
        "unsafe_count",
    ] {
        let directory = f.packet(variant, true, |_, _, record| match variant {
            "foreign" => record["PRIVATE_SENTINEL"] = json!(true),
            "schema" => record["schema_version"] = json!(5),
            "shared_duplicate" => {
                let donor = record["shared_inputs"][0].clone();
                record["shared_inputs"].as_array_mut().unwrap().push(donor);
            }
            "shared_foreign" => record["shared_inputs"][0]["path"] = json!("other.json"),
            "shared_missing" => {
                record["shared_inputs"].as_array_mut().unwrap().pop();
            }
            "logical_limit" => record["aggregate_logical_file_limit"] = json!(SOURCE.expanded + 1),
            "logical_payload" => record["aggregate_logical_file_bytes"] = json!(1),
            "logical_count" => record["logical_reference_files"] = json!(112),
            "physical_count" => record["reference_transport_files"] = json!(113),
            "privacy" => record["private_material_included"] = json!(true),
            "provider" => record["provider_calls"] = json!(1),
            "unsafe_count" => record["reference_files"] = json!(111.5),
            _ => {}
        });
        if ["missing_archive", "expansion", "legacy"].contains(&variant) {
            let mut record: Value = serde_json::from_slice(
                &fs::read(directory.join("transfer-manifest.json")).unwrap(),
            )
            .unwrap();
            match variant {
                "missing_archive" => {
                    record["archives"]
                        .as_object_mut()
                        .unwrap()
                        .remove(DELTA_NAME);
                }
                "expansion" => record["aggregate_expanded_tar_bytes"] = json!(1),
                "legacy" => record["legacy_full_tar_bytes"] = json!(1),
                _ => unreachable!(),
            }
            Fixture::metadata(&directory, &record, DELTA_NAME);
        }
        f.reject(&directory, &format!("{variant}-output"));
    }
    let directory = f.packet("duplicate-json", true, |_, _, _| {});
    let raw = fs::read_to_string(directory.join("transfer-manifest.json")).unwrap();
    let duplicate = raw.replacen('{', "{\"schema_version\":2,", 1);
    Fixture::raw_metadata(&directory, duplicate.as_bytes(), DELTA_NAME);
    f.reject(&directory, "duplicate-output");
    assert!(metadata::parse(br#"{"a":{"b":1,"b":1}}"#, FILE_LIMIT).is_err());
    assert!(
        metadata::parse(
            br#"{"schema_version":2,"\u0073chema_version":2}"#,
            FILE_LIMIT
        )
        .is_err()
    );
    assert!(
        metadata::parse(br#"{"PRIVATE_SENTINEL":"x"}"#, 2)
            .unwrap_err()
            .contains("limit")
    );
    let bytes = vec![0; FILE_LIMIT];
    assert!(sum(std::iter::repeat_n(bytes.as_slice(), 33)).is_err());
}
#[test]
fn checksums_truncation_outer_bounds_and_input_links_fail_before_writes() {
    let f = Fixture::new();
    for variant in [
        "archive",
        "sidecar",
        "resume",
        "sums",
        "truncated",
        "oversize",
        "hardlink",
        "symlink",
    ] {
        let directory = f.packet(variant, true, |_, _, _| {});
        let archive = directory.join(DELTA_NAME);
        match variant {
            "archive" => {
                let mut bytes = fs::read(&archive).unwrap();
                bytes[0] ^= 1;
                fs::write(&archive, bytes).unwrap();
            }
            "sidecar" => fs::write(directory.join(format!("{DELTA_NAME}.sha256")), b"bad").unwrap(),
            "resume" => fs::write(directory.join("RESUME.md"), b"changed").unwrap(),
            "sums" => fs::write(directory.join("SHA256SUMS"), b"bad").unwrap(),
            "truncated" => fs::write(&archive, b"gzip").unwrap(),
            "oversize" => fs::write(directory.join("RESUME.md"), vec![0; META_LIMIT + 1]).unwrap(),
            "hardlink" => fs::hard_link(&archive, directory.join("second-link")).unwrap(),
            "symlink" => {
                fs::rename(&archive, directory.join("saved")).unwrap();
                symlink("saved", &archive).unwrap();
            }
            _ => unreachable!(),
        }
        if variant == "truncated" {
            let mut record: Value = serde_json::from_slice(
                &fs::read(directory.join("transfer-manifest.json")).unwrap(),
            )
            .unwrap();
            record["archives"][DELTA_NAME]["sha256"] = json!(sha(&fs::read(&archive).unwrap()));
            Fixture::metadata(&directory, &record, DELTA_NAME);
            let error = load(&f.root, &directory)
                .err()
                .expect("truncated archive cannot load");
            assert!(["Invalid gzip archive", "Incomplete tar archive"].contains(&error.as_str()));
        }
        f.reject(&directory, &format!("{variant}-output"));
    }
}
#[test]
fn restore_cancel_and_final_or_ancestor_replacement_never_publish_success() {
    let f = Fixture::new();
    let directory = f.packet("valid", true, |_, _, _| {});
    let output = f.scratch.0.join("pre-cancel");
    assert!(
        restore_with(
            &f.root,
            &directory,
            &output,
            || Err("cancelled".into()),
            |_| Ok(())
        )
        .is_err()
    );
    assert!(!output.exists());
    let output = f.scratch.0.join("mid-cancel");
    let mut calls = 0;
    assert!(
        restore_with(
            &f.root,
            &directory,
            &output,
            || {
                calls += 1;
                if calls == 5 {
                    Err("cancelled".into())
                } else {
                    Ok(())
                }
            },
            |_| Ok(())
        )
        .is_err()
    );
    assert!(output.is_dir());
    for ancestor in [false, true] {
        let parent = f
            .scratch
            .0
            .join(if ancestor { "ancestor" } else { "final" });
        fs::create_dir(&parent).unwrap();
        let output = parent.join("restored");
        assert!(
            restore_with(
                &f.root,
                &directory,
                &output,
                || Ok(()),
                |path| {
                    if ancestor {
                        fs::rename(&parent, parent.with_extension("moved")).unwrap();
                        fs::create_dir(&parent).unwrap();
                        fs::create_dir(path).unwrap();
                    } else {
                        fs::rename(path, parent.join("moved")).unwrap();
                        fs::create_dir(path).unwrap();
                    }
                    fs::write(path.join("foreign"), b"keep").unwrap();
                    Ok(())
                }
            )
            .is_err()
        );
        assert_eq!(fs::read(output.join("foreign")).unwrap(), b"keep");
    }
}

#[test]
fn archive_root_and_stream_failure_remain_rejected_with_recomputed_checksums() {
    use std::io::{Read, Write};
    let f = Fixture::new();
    for variant in ["wrong-root", "duplicate-member", "foreign-directory"] {
        let directory = f.packet(variant, true, |_, _, _| {});
        let mut record: Value =
            serde_json::from_slice(&fs::read(directory.join("transfer-manifest.json")).unwrap())
                .unwrap();
        let bytes = if variant == "wrong-root" {
            let mut files = f.reference.clone();
            for (path, _, _) in SHARED {
                files.remove(*path);
            }
            encode_root("foreign-root", &files, SOURCE).unwrap()
        } else {
            let compressed = fs::read(directory.join(DELTA_NAME)).unwrap();
            let mut raw = Vec::new();
            flate2::read::MultiGzDecoder::new(compressed.as_slice())
                .read_to_end(&mut raw)
                .unwrap();
            // First entry is the physical LICENSE member; duplicate its complete
            // header/payload before original contents. Existing bounded decoder
            // rejects duplicate paths rather than silently choosing a payload.
            let padded = 512 + f.reference["LICENSE"].bytes.len().div_ceil(512) * 512;
            let duplicate = if variant == "foreign-directory" {
                let mut header = raw[..512].to_vec();
                header[..100].fill(0);
                let path = format!("{DELTA_ROOT}/foreign");
                header[..path.len()].copy_from_slice(path.as_bytes());
                header[124..136].copy_from_slice(b"00000000000\0");
                header[156] = b'5';
                header[148..156].fill(b' ');
                let checksum: u64 = header.iter().map(|byte| u64::from(*byte)).sum();
                header[148..156].copy_from_slice(format!("{checksum:06o}\0 ").as_bytes());
                let expanded = record["archives"][DELTA_NAME]["expanded_tar_bytes"]
                    .as_u64()
                    .unwrap()
                    + 512;
                record["archives"][DELTA_NAME]["expanded_tar_bytes"] = json!(expanded);
                let stored = record["aggregate_expanded_tar_bytes"].as_u64().unwrap() + 512;
                record["aggregate_expanded_tar_bytes"] = json!(stored);
                record["legacy_full_tar_bytes"] = json!(stored + 79360);
                record["legacy_full_tar_within_limit"] =
                    json!(stored + 79360 <= SOURCE.expanded as u64);
                [header.as_slice(), raw.as_slice()].concat()
            } else {
                [&raw[..padded], raw.as_slice()].concat()
            };
            let mut writer =
                flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
            writer.write_all(&duplicate).unwrap();
            writer.finish().unwrap()
        };
        record["archives"][DELTA_NAME]["sha256"] = json!(sha(&bytes));
        fs::write(directory.join(DELTA_NAME), bytes).unwrap();
        Fixture::metadata(&directory, &record, DELTA_NAME);
        if variant == "foreign-directory" {
            let error = load(&f.root, &directory)
                .err()
                .expect("directory tar member cannot load");
            assert_eq!(
                error,
                "Paired transfer archives require regular members only"
            );
        }
        f.reject(&directory, &format!("{variant}-output"));
    }
}

#[test]
fn legacy_native_fixture_identity_cannot_be_null_missing_or_non_digest() {
    let f = Fixture::new();
    for variant in ["missing-and-null", "null-present", "non-digest"] {
        let directory = f.packet(variant, false, |native, _, _| {
            let mut record: Value =
                serde_json::from_slice(&native["source-manifest.json"].bytes).unwrap();
            if variant == "missing-and-null" {
                native.remove("test/fixtures/ollama-routing.json").unwrap();
                record["files"]
                    .as_object_mut()
                    .unwrap()
                    .remove("test/fixtures/ollama-routing.json");
            }
            record["fixture_sha256"] = if variant == "non-digest" {
                json!({"wrong":"shape"})
            } else {
                Value::Null
            };
            native.get_mut("source-manifest.json").unwrap().bytes =
                serde_json::to_vec(&record).unwrap();
        });
        f.reject(&directory, &format!("{variant}-output"));
    }
}

#[test]
fn schema3_complete_reference_restore_and_independent_legacy_tar_charge_are_exact() {
    let f = Fixture::new();
    let directory = f.packet("schema3", true, |_, _, _| {});
    let old: Value =
        serde_json::from_slice(&fs::read(directory.join("transfer-manifest.json")).unwrap())
            .unwrap();
    f.schema3(&directory);
    let plan = load(&f.root, &directory).unwrap();
    assert_eq!(plan.record["schema_version"], 3);
    assert_eq!(
        plan.record["legacy_full_tar_bytes"],
        old["legacy_full_tar_bytes"]
    );
    assert_eq!(plan.native.files.len(), f.native.len());
    for (path, entry) in &f.native {
        assert_eq!(plan.native.files[path].bytes, entry.bytes);
        assert_eq!(plan.native.files[path].mode, entry.mode);
    }
    assert_eq!(
        plan.record["aggregate_expanded_carrier_bytes"].as_u64(),
        Some((plan.native.expanded_bytes + plan.reference.expanded_bytes) as u64)
    );
    let output = f.scratch.0.join("schema3-restored");
    let report = restore(&f.root, &directory, &output).unwrap();
    assert_eq!(report["transfer_schema"], 3);
    assert_eq!(report["logical_files"], 113);
    assert_eq!(report["reference_files"], 111);
    for (path, entry) in &f.reference {
        assert_eq!(fs::read(output.join(path)).unwrap(), entry.bytes);
        assert_eq!(
            fs::metadata(output.join(path))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            entry.mode
        );
    }
    assert!(restore(&f.root, &directory, &output).is_err());
}
#[test]
fn schema3_format_names_dimensions_unknown_fields_and_legacy_fallback_are_rejected() {
    let f = Fixture::new();
    for variant in [
        "format",
        "old_dimension",
        "wrong_charge",
        "extra_descriptor",
        "native_name",
        "shared_omission",
        "logical_charge",
        "legacy_charge",
    ] {
        let directory = f.packet(variant, true, |_, _, _| {});
        f.schema3(&directory);
        let mut record: Value =
            serde_json::from_slice(&fs::read(directory.join("transfer-manifest.json")).unwrap())
                .unwrap();
        match variant {
            "format" => record["archives"][CPIO_NAME]["format"] = json!("gzip-ustar"),
            "old_dimension" => {
                let old = record
                    .as_object_mut()
                    .unwrap()
                    .remove("aggregate_expanded_carrier_bytes")
                    .unwrap();
                record["aggregate_expanded_tar_bytes"] = old;
            }
            "wrong_charge" => record["aggregate_expanded_carrier_bytes"] = json!(1),
            "extra_descriptor" => record["archives"][CPIO_NAME]["PRIVATE_FIELD"] = json!(true),
            "native_name" => {
                let old = record["archives"]
                    .as_object_mut()
                    .unwrap()
                    .remove(CPIO_NAME)
                    .unwrap();
                record["archives"]["native-source.tar.gz"] = old;
            }
            "shared_omission" => {
                record["shared_inputs"].as_array_mut().unwrap().pop();
            }
            "logical_charge" => record["aggregate_logical_file_bytes"] = json!(1),
            "legacy_charge" => record["legacy_full_tar_bytes"] = json!(1),
            _ => unreachable!(),
        }
        Fixture::v3_metadata(&directory, &record);
        f.reject(&directory, &format!("{variant}-output"));
    }
    let directory = f.packet("v2-body-labelled-v3", true, |_, _, record| {
        record["schema_version"] = json!(3)
    });
    f.reject(&directory, "no-fallback-output");
}
#[test]
fn schema3_truncated_or_nonzero_after_trailer_inputs_fail_before_destination_creation() {
    use std::io::{Read, Write};
    let f = Fixture::new();
    for variant in ["truncated", "after-trailer"] {
        let directory = f.packet(variant, true, |_, _, _| {});
        f.schema3(&directory);
        let mut record: Value =
            serde_json::from_slice(&fs::read(directory.join("transfer-manifest.json")).unwrap())
                .unwrap();
        let bytes = if variant == "truncated" {
            b"gzip".to_vec()
        } else {
            let encoded = fs::read(directory.join(CPIO_NAME)).unwrap();
            let mut bytes = Vec::new();
            flate2::read::MultiGzDecoder::new(encoded.as_slice())
                .read_to_end(&mut bytes)
                .unwrap();
            bytes.push(1);
            let mut writer = flate2::GzBuilder::new()
                .mtime(0)
                .write(Vec::new(), flate2::Compression::default());
            writer.write_all(&bytes).unwrap();
            writer.finish().unwrap()
        };
        record["archives"][CPIO_NAME]["sha256"] = json!(sha(&bytes));
        fs::write(directory.join(CPIO_NAME), bytes).unwrap();
        Fixture::v3_metadata(&directory, &record);
        let error = load(&f.root, &directory)
            .err()
            .expect("actual CPIO decoder must reject");
        assert!(if variant == "truncated" {
            ["Invalid gzip archive", "Incomplete CPIO archive"].contains(&error.as_str())
        } else {
            error == "Nonzero data after CPIO trailer"
        });
        f.reject(&directory, &format!("{variant}-output"));
    }
}

#[test]
fn schema4_reuses_only_pinned_extras_preserving_every_logical_byte_mode_and_legacy_charge() {
    let f = Fixture::new();
    let directory = f.packet("schema4", true, |_, _, _| {});
    f.schema3(&directory);
    let old = load(&f.root, &directory).unwrap();
    // The schema4 helper starts from a schema2 packet; keep the admitted schema3
    // record above as an independent unchanged-format comparison.
    let directory = f.packet("schema4-new", true, |_, _, _| {});
    f.schema4(&directory, |_, _, _| {});
    let plan = load(&f.root, &directory).unwrap();
    assert_eq!(plan.record["schema_version"], 4);
    assert_eq!(plan.record["shared_inputs"].as_array().unwrap().len(), 4);
    assert_eq!(plan.reference.files.len(), 109);
    assert!(!plan.reference.files.contains_key("LICENSE"));
    assert!(
        !plan
            .reference
            .files
            .contains_key("frozen-reference-manifest.json")
    );
    assert_eq!(
        plan.record["aggregate_logical_file_bytes"],
        old.record["aggregate_logical_file_bytes"]
    );
    assert_eq!(plan.record["aggregate_logical_file_limit"], SOURCE.expanded);
    assert_eq!(plan.record["aggregate_expanded_limit"], SOURCE.expanded);
    assert_eq!(
        plan.record["legacy_full_tar_bytes"],
        old.record["legacy_full_tar_bytes"]
    );
    let removed = ["LICENSE", "frozen-reference-manifest.json"]
        .into_iter()
        .map(|path| 512 + f.reference[path].bytes.len().div_ceil(512) * 512)
        .sum::<usize>();
    assert_eq!(removed, 99840);
    assert_eq!(
        old.reference.expanded_bytes - plan.reference.expanded_bytes,
        removed
    );
    assert_eq!(plan.native.files.len(), old.native.files.len());
    for (path, entry) in &old.native.files {
        assert_eq!(plan.native.files[path].bytes, entry.bytes);
        assert_eq!(plan.native.files[path].mode, entry.mode);
    }
    let output = f.scratch.0.join("schema4-restored");
    let report = restore(&f.root, &directory, &output).unwrap();
    assert_eq!(report["logical_files"], 113);
    assert_eq!(report["reference_files"], 111);
    assert_eq!(report["transfer_schema"], 4);
    for (path, entry) in &f.reference {
        assert_eq!(fs::read(output.join(path)).unwrap(), entry.bytes);
        assert_eq!(
            fs::metadata(output.join(path))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            entry.mode
        );
    }
    assert!(restore(&f.root, &directory, &output).is_err());
}
#[test]
fn schema4_changed_missing_duplicate_or_unpinned_donors_and_relaxed_charges_fail_before_writes() {
    let f = Fixture::new();
    let mut reference = f.reference.clone();
    for path in [
        SHARED[0].0,
        SHARED[1].0,
        "LICENSE",
        "frozen-reference-manifest.json",
    ] {
        reference.remove(path).unwrap();
    }
    for path in ["LICENSE", "rust/parity/baseline.json"] {
        let mut native = f.native.clone();
        native.get_mut(path).unwrap().mode = 0o600;
        assert!(
            logical(
                &native,
                &reference,
                &f.reference["frozen-reference-manifest.json"].bytes,
                4
            )
            .is_err()
        );
    }
    for variant in [
        "license_missing",
        "license_changed",
        "baseline_changed",
        "duplicate_license",
        "duplicate_manifest",
        "missing_license_label",
        "missing_manifest_alias",
        "foreign_alias",
        "alias_digest",
        "alias_field",
        "logical_count",
        "physical_count",
        "logical_bytes",
        "logical_limit",
        "carrier_limit",
        "legacy_charge",
        "downgrade",
    ] {
        let directory = f.packet(variant, true, |_, _, _| {});
        f.schema4(&directory, |native, reference, record| match variant {
            "license_missing" => {
                native.remove("LICENSE").unwrap();
                Fixture::manifest(native);
            }
            "license_changed" => {
                native.get_mut("LICENSE").unwrap().bytes[0] ^= 1;
                Fixture::manifest(native);
            }
            "baseline_changed" => {
                native.get_mut("rust/parity/baseline.json").unwrap().bytes[0] ^= 1;
                Fixture::manifest(native);
            }
            "duplicate_license" => {
                reference.insert("LICENSE".into(), f.reference["LICENSE"].clone());
            }
            "duplicate_manifest" => {
                reference.insert(
                    "frozen-reference-manifest.json".into(),
                    f.reference["frozen-reference-manifest.json"].clone(),
                );
            }
            "missing_license_label" => {
                record["shared_inputs"].as_array_mut().unwrap().remove(2);
            }
            "missing_manifest_alias" => {
                record["shared_inputs"].as_array_mut().unwrap().pop();
            }
            "foreign_alias" => record["shared_inputs"][3]["source_path"] = json!("foreign.json"),
            "alias_digest" => record["shared_inputs"][3]["sha256"] = json!("0".repeat(64)),
            "alias_field" => record["shared_inputs"][3]["PRIVATE_SENTINEL"] = json!(true),
            "logical_count" => record["logical_reference_files"] = json!(111),
            "physical_count" => record["reference_transport_files"] = json!(111),
            "logical_bytes" => record["aggregate_logical_file_bytes"] = json!(1),
            "logical_limit" => record["aggregate_logical_file_limit"] = json!(SOURCE.expanded + 1),
            "carrier_limit" => record["aggregate_expanded_limit"] = json!(SOURCE.expanded + 1),
            "legacy_charge" => record["legacy_full_tar_bytes"] = json!(1),
            "downgrade" => record["schema_version"] = json!(3),
            _ => unreachable!(),
        });
        f.reject(&directory, &format!("{variant}-output"));
    }
}
