// Schema7: joint raw logical/carrier budgets only; no product caps change.
#[test]
fn schema7_joint_bounds_count_all_raw_copies_and_preserve_archive_policies() {
    use crate::package::archive::{HISTORICAL, NATIVE};
    let mib = 1024 * 1024;
    assert_eq!((SOURCE.compressed, SOURCE.expanded), (32 * mib, 32 * mib));
    assert_eq!(
        (HISTORICAL.compressed, HISTORICAL.expanded),
        (32 * mib, 32 * mib)
    );
    assert_eq!(
        (NATIVE.compressed, NATIVE.expanded, NATIVE.entries),
        (32 * mib, 64 * mib, 256)
    );
    for schema in 1..=6 {
        assert_eq!(joint_limit(schema).unwrap(), 32 * mib);
    }
    assert_eq!(joint_limit(7).unwrap(), 40 * mib);
    for schema in [0, 8, u64::MAX] {
        assert!(joint_limit(schema).is_err());
    }
    let chunk = vec![b'x'; mib];
    // Repeated borrows are still separate raw logical copies, never deduplicated.
    assert_eq!(
        schema_sum(std::iter::repeat_n(chunk.as_slice(), 40), 7).unwrap(),
        40 * mib
    );
    assert!(
        schema_sum(
            std::iter::repeat_n(chunk.as_slice(), 40).chain([b"x".as_slice()]),
            7
        )
        .is_err()
    );
    for schema in 1..=6 {
        assert_eq!(
            schema_sum(std::iter::repeat_n(chunk.as_slice(), 32), schema).unwrap(),
            32 * mib
        );
        assert!(
            schema_sum(
                std::iter::repeat_n(chunk.as_slice(), 32).chain([b"x".as_slice()]),
                schema
            )
            .is_err()
        );
    }
    // Actual shared receiver/producer arithmetic: exact40, +1 and per-archive cap.
    assert_eq!(carrier_total(7, 8 * mib, 32 * mib).unwrap(), 40 * mib);
    assert!(carrier_total(7, 8 * mib + 1, 32 * mib).is_err());
    assert!(carrier_total(7, 32 * mib + 1, 0).is_err());
    assert!(carrier_total(7, 0, 32 * mib + 1).is_err());
    assert_eq!(reference_policy(7, 8 * mib - 1).unwrap().expanded, 32 * mib);
    assert_eq!(
        reference_policy(7, 8 * mib + 1).unwrap().expanded,
        32 * mib - 1
    );
    for schema in 1..=6 {
        assert_eq!(carrier_total(schema, 8 * mib, 24 * mib).unwrap(), 32 * mib);
        assert!(carrier_total(schema, 8 * mib + 1, 24 * mib).is_err());
    }
}

#[test]
fn schema7_receiver_restores_exact_reference_and_rejects_budget_or_version_forgery() {
    let f = Fixture::new();
    for variant in [
        "good7",
        "old6-relaxed",
        "new7-legacy",
        "expanded-plus1",
        "logical-plus1",
        "unknown8",
    ] {
        let directory = f.packet(variant, true, |_, _, _| {});
        f.schema4(&directory, |native, reference, record| {
            record["schema_version"] = json!(6);
            let original = native.remove("source-manifest.json").unwrap();
            let facts = metadata::parse(&original.bytes, FILE_LIMIT).unwrap();
            native.insert(
                "source-manifest.bin".into(),
                Entry {
                    bytes: metadata::encode_source(&facts, FILE_LIMIT).unwrap(),
                    mode: original.mode,
                },
            );
            record["aggregate_logical_file_bytes"] = json!(
                sum(native
                    .values()
                    .map(|e| e.bytes.as_slice())
                    .chain(f.reference.values().map(|e| e.bytes.as_slice())))
                .unwrap()
            );
            let legacy = legacy_full(native, expanded_size(reference, SOURCE).unwrap(), 6).unwrap();
            record["legacy_full_tar_bytes"] = json!(legacy);
            record["legacy_full_tar_within_limit"] = json!(legacy <= SOURCE.expanded);
        });
        let mut record = read_record(&directory);
        record["schema_version"] = json!(7);
        record["aggregate_expanded_limit"] = json!(40 * 1024 * 1024);
        record["aggregate_logical_file_limit"] = json!(40 * 1024 * 1024);
        match variant {
            "old6-relaxed" => record["schema_version"] = json!(6),
            "new7-legacy" => record["aggregate_expanded_limit"] = json!(SOURCE.expanded),
            "expanded-plus1" => record["aggregate_expanded_limit"] = json!(40 * 1024 * 1024 + 1),
            "logical-plus1" => record["aggregate_logical_file_limit"] = json!(40 * 1024 * 1024 + 1),
            "unknown8" => record["schema_version"] = json!(8),
            _ => {}
        }
        Fixture::v3_metadata(&directory, &record);
        if variant == "logical-plus1" {
            // Invalid schema7 budgets fail before even opening the payload.
            fs::remove_file(directory.join(CPIO_NAME)).unwrap();
            assert_eq!(
                load(&f.root, &directory).err().unwrap(),
                "Logical paired transfer declaration differs"
            );
        }
        if variant != "good7" {
            f.reject(&directory, &format!("{variant}-output"));
            continue;
        }
        let plan = load(&f.root, &directory).unwrap();
        assert_eq!(plan.schema, 7);
        assert_eq!(plan.native.files.len(), f.native.len());
        for (path, wanted) in &f.native {
            if path != "source-manifest.json" {
                assert_eq!(plan.native.files[path].bytes, wanted.bytes);
                assert_eq!(plan.native.files[path].mode, wanted.mode);
            }
        }
        let output = f.scratch.0.join("schema7-restored");
        let restored = restore(&f.root, &directory, &output).unwrap();
        assert_eq!(restored["transfer_schema"], 7);
        assert_eq!(restored["reference_files"], 111);
        assert_eq!(restored["logical_files"], 113);
        assert_restored(&output, &f.reference);
        assert!(restore(&f.root, &directory, &output).is_err());
    }
}

#[test]
fn schema7_current_source_producer_receiver_keep_every_raw_input_and_frozen_file() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap();
    let scratch = Scratch::new("schema7-current-source").unwrap();
    let directory = scratch.0.join("transfer");
    let expected = collect(&root).unwrap();
    let report = create_schema7(&root, &directory).unwrap();
    assert_eq!(report["schema_version"], 7);
    assert_eq!(report["aggregate_expanded_limit"], 40 * 1024 * 1024);
    assert_eq!(report["aggregate_logical_file_limit"], 40 * 1024 * 1024);
    let plan = load(&root, &directory).unwrap();
    assert!(plan.native.expanded_bytes <= SOURCE.expanded);
    assert!(plan.reference.expanded_bytes <= SOURCE.expanded);
    assert_eq!(plan.native.files.len(), expected.len());
    assert_eq!(
        canonical_source(&plan.native.files, FILE_LIMIT).unwrap(),
        expected["source-manifest.json"].bytes
    );
    for (path, wanted) in expected {
        if path == "source-manifest.json" {
            continue;
        }
        assert_eq!(plan.native.files[&path].bytes, wanted.bytes, "{path}");
        assert_eq!(plan.native.files[&path].mode, wanted.mode, "{path}");
    }
    let output = scratch.0.join("reference");
    assert_eq!(
        restore(&root, &directory, &output).unwrap()["logical_files"],
        113
    );
    let baseline: Value =
        serde_json::from_slice(&fs::read(root.join("rust/parity/baseline.json")).unwrap()).unwrap();
    let rows = baseline["files"].as_array().unwrap();
    assert_eq!(rows.len(), 111);
    for row in rows {
        let bytes = fs::read(output.join(row["path"].as_str().unwrap())).unwrap();
        assert_eq!(bytes.len() as u64, row["bytes"].as_u64().unwrap());
        assert_eq!(sha(&bytes), row["sha256"]);
    }
    assert_eq!(
        fs::read(output.join("LICENSE")).unwrap(),
        fs::read(root.join("LICENSE")).unwrap()
    );
    assert_eq!(
        fs::read(output.join("frozen-reference-manifest.json")).unwrap(),
        fs::read(root.join("rust/parity/baseline.json")).unwrap()
    );
    assert!(create_schema7(&root, &directory).is_err());

    // Reuse the exact archives with valid legacy declarations and checksums.
    // Size policy must still come from schema6, even when the bytes came from7.
    let mut legacy = read_record(&directory);
    legacy["schema_version"] = json!(6);
    legacy["aggregate_expanded_limit"] = json!(SOURCE.expanded);
    legacy["aggregate_logical_file_limit"] = json!(SOURCE.expanded);
    fs::write(directory.join("RESUME.md"), super::super::transfer::README).unwrap();
    Fixture::v3_metadata(&directory, &legacy);
    let legacy_output = scratch.0.join("legacy-reference");
    let outcome = restore(&root, &directory, &legacy_output);
    if report["aggregate_expanded_carrier_bytes"].as_u64().unwrap() > SOURCE.expanded as u64
        || report["aggregate_logical_file_bytes"].as_u64().unwrap() > SOURCE.expanded as u64
    {
        assert!(outcome.is_err());
        assert!(!legacy_output.exists());
    } else {
        // A future smaller checkout may legitimately fit the old format.
        assert_eq!(outcome.unwrap()["logical_files"], 113);
    }
}
