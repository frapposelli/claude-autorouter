//! Synthetic declarations and executable headers test rejection, never qualification.
use super::*;
use crate::tool_process::Scratch;
#[path = "release_native_fixture.rs"]
mod cli_fixture;
fn material(dir: &Scratch, name: &str, bytes: &[u8]) -> Value {
    dir.file(name, bytes).unwrap();
    json!({"path":name,"sha256":digest(bytes)})
}
fn document(dir: &Scratch, name: &str, value: &Value) -> Value {
    material(dir, name, &json_bytes(value))
}
struct Fixture {
    dir: Scratch,
    inputs: Value,
    source: Value,
    baseline: Value,
    public: BTreeMap<String, Entry>,
    public_sha: String,
}
impl Fixture {
    fn new() -> Self {
        let source = json!({"commit":"a".repeat(40),"dirty":false,"cargo_lock_sha256":"b".repeat(64),"provenance":"ci-source-build"});
        Self::with_source(source)
    }
    fn with_source(source: Value) -> Self {
        let raw = cli_fixture::MaterialFixture::new(source);
        let dir = Scratch::new("release-pack-synthetic").unwrap();
        for (name, bytes) in &raw.materials {
            dir.file(name, bytes).unwrap();
        }
        Self {
            dir,
            inputs: raw.inputs,
            source: raw.source,
            baseline: raw.baseline,
            public: raw
                .public
                .into_iter()
                .map(|(path, item)| {
                    (
                        path,
                        Entry {
                            bytes: item.bytes,
                            mode: item.mode,
                        },
                    )
                })
                .collect(),
            public_sha: raw.public_sha,
        }
    }
    fn qualified(&self) -> Result<Qualified, String> {
        qualify(
            &self.dir.0,
            &self.inputs,
            &self.source,
            "0.4.0",
            &self.baseline,
            &self.public_sha,
        )
    }
    fn assembled(&self) -> BTreeMap<String, Vec<u8>> {
        assemble(
            self.public.clone(),
            &self.source,
            "0.4.0",
            self.qualified().unwrap(),
        )
        .unwrap()
    }
    fn replace(&mut self, key: &[&str], value: Value) {
        let mut at = &mut self.inputs;
        for part in key {
            at = &mut at[part];
        }
        *at = document(&self.dir, "mutated.json", &value);
    }
}

#[path = "release_archive_contracts.rs"]
mod archive_contracts;
#[test]
fn production_assembly_is_deterministic_bounded_and_has_no_node_runtime() {
    let fixture = Fixture::new();
    let files = fixture.assembled();
    assert_eq!(files, fixture.assembled());
    assert_eq!(files.len(), 6); // npm, one direct archive, index, and sidecars.
    let (npm, _) = archive::decode(&files["claude-autorouter-0.4.0.tgz"], archive::NATIVE)
        .unwrap()
        .into_parts();
    let manifest = json_read(&npm["package.json"].bytes).unwrap();
    assert!(manifest.get("scripts").is_none());
    assert!(manifest.get("engines").is_none());
    assert!(manifest.get("devDependencies").is_none());
    assert!(
        !npm.keys()
            .any(|p| p.ends_with(".mjs") || p.contains("fixture") || p.contains("AGENTS"))
    );
    for path in release::ROOT_FILES.iter().chain(release::DOCS) {
        assert!(npm.contains_key(*path), "missing {path}");
    }
    let build = release::native_files(&npm, &manifest).unwrap();
    assert_eq!(build["release_approved"], false);
    assert_eq!(build["qualification_approved"], true);
    let report = direct::verify_bytes(
        "claude-autorouter-0.4.0-aarch64-apple-darwin.tar.gz",
        &files["claude-autorouter-0.4.0-aarch64-apple-darwin.tar.gz"],
    )
    .unwrap();
    assert_eq!(
        report["npm"]["sha256"],
        digest(&files["claude-autorouter-0.4.0.tgz"])
    );
    let output = fixture.dir.0.join("output");
    write_output(&output, &files).unwrap();
    assert!(write_output(&output, &files).is_err());
    assert_eq!(
        release::inspect_artifact(&output.join("claude-autorouter-0.4.0.tgz"), "v0.4.0", false)
            .err()
            .unwrap()
            .detail["reason"],
        "native_final_authorization_required"
    );
}
#[test]
fn qualification_rejects_missing_checks_copied_hashes_and_scope_reduction() {
    for mutation in [
        "missing-approval",
        "hash",
        "report-check",
        "report-source",
        "report-evidence",
        "matrix-target",
        "resolution",
        "resolution-evidence",
        "licenses",
        "provenance",
        "duplicate-artifact",
        "missing-artifact",
        "approval-docs",
        "approval-binary",
    ] {
        let mut f = Fixture::new();
        match mutation {
            "missing-approval" => {
                f.inputs.as_object_mut().unwrap().remove("approval");
            }
            "hash" => f.inputs["licenses"]["sha256"] = json!("0".repeat(64)),
            "report-check" | "report-source" | "report-evidence" => {
                let mut r =
                    json_read(&std::fs::read(f.dir.0.join("gate-performance.json")).unwrap())
                        .unwrap();
                match mutation {
                    "report-check" => r["checks"]["paired_live_task_quality"] = json!(false),
                    "report-source" => r["source_commit"] = json!("0".repeat(40)),
                    _ => r["evidence"] = json!([]),
                };
                f.replace(&["qualification", "performance"], r);
            }
            "matrix-target" => f.baseline["targets"]
                .as_array_mut()
                .unwrap()
                .push(json!({"target":"x86_64-apple-darwin"})),
            "resolution" | "resolution-evidence" => {
                let mut m =
                    json_read(&std::fs::read(f.dir.0.join("platforms.json")).unwrap()).unwrap();
                if mutation == "resolution" {
                    m["baseline_resolutions"] = json!([])
                } else {
                    m["baseline_resolutions"][0]["evidence_sha256"] = json!("0".repeat(64))
                };
                f.replace(&["platforms"], m);
            }
            "licenses" => {
                f.dir.file("licenses.txt", b"changed").unwrap();
            }
            "provenance" => {
                let mut p =
                    json_read(&std::fs::read(f.dir.0.join("provenance.json")).unwrap()).unwrap();
                p["rustflags"] = json!(["-Ctarget-cpu=native"]);
                f.inputs["artifacts"][0]["provenance"] =
                    document(&f.dir, "changed-provenance.json", &p);
            }
            "duplicate-artifact" => {
                let row = f.inputs["artifacts"][0].clone();
                f.inputs["artifacts"].as_array_mut().unwrap().push(row);
            }
            "missing-artifact" => f.inputs["artifacts"] = json!([]),
            "approval-docs" => f.public_sha = "0".repeat(64),
            _ => {
                let mut a =
                    json_read(&std::fs::read(f.dir.0.join("approval.json")).unwrap()).unwrap();
                a["artifacts"]["aarch64-apple-darwin"]["binary_sha256"] = json!("0".repeat(64));
                f.replace(&["approval"], a);
            }
        }
        assert!(f.qualified().is_err(), "mutation {mutation}");
    }
}
#[test]
fn unresolved_baseline_architectures_need_distinct_added_targets() {
    // This proves target allocation only; platform evidence must establish
    // whether a target actually supports the named architecture.
    let f = Fixture::new();
    let mut matrix = json_read(&std::fs::read(f.dir.0.join("platforms.json")).unwrap()).unwrap();
    matrix["baseline_resolutions"][0]["result"] = json!("qualified");
    matrix["baseline_resolutions"][0]["target"] = json!("aarch64-apple-darwin");
    assert!(
        validate_matrix(&matrix, &f.baseline)
            .unwrap_err()
            .contains("Baseline architecture must be qualified")
    );

    matrix["targets"]
        .as_array_mut()
        .unwrap()
        .push(json!({"target":"x86_64-apple-darwin","qualification":"passed"}));
    matrix["baseline_resolutions"][0]["target"] = json!("x86_64-apple-darwin");
    assert!(validate_matrix(&matrix, &f.baseline).is_ok());

    let mut baseline = f.baseline.clone();
    baseline["unresolved_baseline_architectures"]
        .as_array_mut()
        .unwrap()
        .push(json!("second synthetic unresolved platform"));
    let mut second = matrix["baseline_resolutions"][0].clone();
    second["architecture"] = json!("second synthetic unresolved platform");
    matrix["baseline_resolutions"]
        .as_array_mut()
        .unwrap()
        .push(second);
    assert!(
        validate_matrix(&matrix, &baseline)
            .unwrap_err()
            .contains("Baseline architecture must be qualified")
    );
}

fn linux_x86_allocation_fixture() -> (Value, Value) {
    // Synthetic allocation evidence only; no checked-in qualification changes.
    let baseline = json_read(include_bytes!("../../distribution/platforms.json")).unwrap();
    let mut matrix = baseline.clone();
    matrix["status"] = json!("release_matrix_approved");
    matrix["unresolved_baseline_architectures"] = json!([]);
    for row in matrix["targets"].as_array_mut().unwrap() {
        row["qualification"] = json!("passed");
    }
    matrix["baseline_resolutions"] = json!(baseline["unresolved_baseline_architectures"]
        .as_array()
        .unwrap()
        .iter()
        .map(|architecture| {
            let mut row = json!({"architecture":architecture,"evidence_sha256":digest(b"synthetic allocation only"),"review_reference":"synthetic:target-allocation"});
            if architecture == "Linux x86" {
                row["result"] = json!("qualified");
                row["target"] = json!("i686-unknown-linux-gnu");
            } else {
                row["result"] = json!("baseline_unavailable");
                row["reason"] = json!("synthetic unavailable baseline fixture");
            }
            row
        })
        .collect::<Vec<_>>());
    (baseline, matrix)
}

#[test]
fn predeclared_linux_x86_target_can_resolve_its_canonical_architecture() {
    let (baseline, matrix) = linux_x86_allocation_fixture();
    let original = baseline["targets"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["target"] == "i686-unknown-linux-gnu")
        .unwrap();
    assert_eq!(original["candidate"], false);
    assert_eq!(original["qualification"], "pending");
    assert!(
        validate_matrix(&matrix, &baseline)
            .unwrap()
            .contains("i686-unknown-linux-gnu")
    );
    // The ordinary pending matrix remains inadmissible despite the mapping.
    assert!(validate_matrix(&baseline, &baseline).is_err());
}

#[test]
fn predeclared_resolution_preserves_scope_evidence_uniqueness_and_qualification_guards() {
    let (baseline, original) = linux_x86_allocation_fixture();
    for mutation in [
        "unrelated-target",
        "other-architecture",
        "shared-target",
        "pending-target",
        "unapproved",
        "unresolved",
        "drop-target",
        "caps",
        "baseline",
        "missing-resolution",
        "duplicate-resolution",
        "evidence",
        "review",
    ] {
        let mut matrix = original.clone();
        let resolutions = matrix["baseline_resolutions"].as_array().unwrap();
        let x86 = resolutions
            .iter()
            .position(|row| row["architecture"] == "Linux x86")
            .unwrap();
        let other = resolutions
            .iter()
            .position(|row| row["architecture"] != "Linux x86")
            .unwrap();
        match mutation {
            "unrelated-target" => {
                matrix["baseline_resolutions"][x86]["target"] = json!("x86_64-unknown-linux-gnu")
            }
            "other-architecture" | "shared-target" => {
                matrix["baseline_resolutions"][other]["result"] = json!("qualified");
                matrix["baseline_resolutions"][other]["target"] = json!("i686-unknown-linux-gnu");
                if mutation == "other-architecture" {
                    matrix["baseline_resolutions"][x86]["result"] = json!("baseline_unavailable");
                    matrix["baseline_resolutions"][x86]["reason"] = json!("synthetic fixture");
                }
            }
            "pending-target" => {
                matrix["targets"]
                    .as_array_mut()
                    .unwrap()
                    .iter_mut()
                    .find(|row| row["target"] == "i686-unknown-linux-gnu")
                    .unwrap()["qualification"] = json!("pending")
            }
            "unapproved" => matrix["status"] = baseline["status"].clone(),
            "unresolved" => matrix["unresolved_baseline_architectures"] = json!(["Linux x86"]),
            "drop-target" => matrix["targets"]
                .as_array_mut()
                .unwrap()
                .retain(|row| row["target"] != "i686-unknown-linux-gnu"),
            "caps" => {
                matrix["artifact_caps"]["expanded_tar_bytes"] =
                    json!(archive::MAX_NATIVE_EXPANDED + 1)
            }
            "baseline" => matrix["baseline"]["cpu_restriction"] = json!("synthetic restriction"),
            "missing-resolution" => {
                matrix["baseline_resolutions"]
                    .as_array_mut()
                    .unwrap()
                    .remove(x86);
            }
            "duplicate-resolution" => {
                let row = matrix["baseline_resolutions"][x86].clone();
                matrix["baseline_resolutions"]
                    .as_array_mut()
                    .unwrap()
                    .push(row);
            }
            "evidence" => matrix["baseline_resolutions"][x86]["evidence_sha256"] = json!("invalid"),
            "review" => matrix["baseline_resolutions"][x86]["review_reference"] = json!(""),
            _ => unreachable!(),
        }
        assert!(validate_matrix(&matrix, &baseline).is_err(), "{mutation}");
    }
}
#[test]
fn direct_verifier_rejects_foreign_architecture_runtime_and_undeclared_docs() {
    let f = Fixture::new();
    let output = f.assembled();
    let name = "claude-autorouter-0.4.0-aarch64-apple-darwin.tar.gz";
    for mutation in [
        "foreign-cpu",
        "unknown-file",
        "missing-doc",
        "mode",
        "npm-hash",
        "self-approved",
    ] {
        let (mut files, _) = archive::decode_root(&output[name], PACKAGE, archive::NATIVE)
            .unwrap()
            .into_parts();
        match mutation {
            "foreign-cpu" => files.get_mut("bin/claude-autorouter").unwrap().bytes[4..8]
                .copy_from_slice(&0x01000007u32.to_le_bytes()),
            "unknown-file" => insert(&mut files, "src/runtime.mjs", b"node".to_vec(), 0o644),
            "missing-doc" => {
                files.remove("docs/reference.md");
            }
            "mode" => files.get_mut("bin/claude-autorouter").unwrap().mode = 0o644,
            _ => {
                let mut build = json_read(&files["build-manifest.json"].bytes).unwrap();
                if mutation == "npm-hash" {
                    build["npm"]["sha256"] = json!("bad")
                } else {
                    build["release_approved"] = json!(true)
                };
                files.get_mut("build-manifest.json").unwrap().bytes = json_bytes(&build);
            }
        }
        let archive = archive::encode_root(PACKAGE, &files, archive::NATIVE).unwrap();
        assert!(direct::verify_bytes(name, &archive).is_err(), "{mutation}");
    }
}
#[test]
fn production_source_allowlist_refuses_final_symlinks_and_omits_adjacent_files() {
    let f = Fixture::new();
    std::fs::create_dir(f.dir.0.join("docs")).unwrap();
    for (name, entry) in &f.public {
        std::fs::write(f.dir.0.join(name), &entry.bytes).unwrap();
    }
    f.dir.file("private-canary.txt", b"do not include").unwrap();
    let files = public_files(&f.dir.0).unwrap();
    assert!(!files.contains_key("private-canary.txt"));
    #[cfg(unix)]
    {
        std::fs::remove_file(f.dir.0.join("README.md")).unwrap();
        std::os::unix::fs::symlink("private-canary.txt", f.dir.0.join("README.md")).unwrap();
        assert!(public_files(&f.dir.0).is_err());
    }
}
#[test]
fn final_authorization_requires_exact_archive_bytes_and_all_distribution_checks() {
    let f = Fixture::new();
    let output = f.dir.0.join("output");
    write_output(&output, &f.assembled()).unwrap();
    let candidate = authorization::candidate(&output).unwrap();
    let evidence = material(
        &f.dir,
        "installed-evidence.json",
        b"synthetic installed report fixture only",
    );
    let mut rows = Vec::new();
    for expected in candidate.expected.values() {
        let mut row = expected.clone();
        row["passed"] = json!(true);
        row["checks"] = json!({});
        for key in authorization::SHARED
            .iter()
            .chain(["offline_install_no_scripts", "upgrade_rollback"].iter())
        {
            row["checks"][key] = json!(true)
        }
        row["evidence"] = json!([evidence]);
        rows.push(row);
    }
    let report = json!({"schema_version":1,"kind":"native_release_final_qualification","passed":true,"complete":true,"source":f.source,"release_index_sha256":candidate.index_hash,"instances":rows});
    let report_desc = document(&f.dir, "final-report.json", &report);
    let approval = json!({"schema_version":1,"kind":"native_release_final_approval","approved":true,"source":f.source,"release_index_sha256":candidate.index_hash,"report_sha256":report_desc["sha256"],"reviewer":"synthetic fixture","review_reference":"synthetic:test"});
    let approval_desc = document(&f.dir, "final-approval.json", &approval);
    let input = json!({"schema_version":1,"kind":"native_release_final_inputs","report":report_desc,"approval":approval_desc});
    let auth = authorization::authorize(&f.dir.0, &input, &candidate).unwrap();
    assert!(auth.to_string().find(f.dir.0.to_str().unwrap()).is_none());
    for mutation in [
        "missing-direct",
        "changed-archive",
        "failed-upgrade",
        "failed-direct",
        "missing-evidence",
    ] {
        let mut changed = report.clone();
        match mutation {
            "missing-direct" => {
                changed["instances"]
                    .as_array_mut()
                    .unwrap()
                    .retain(|r| r["distribution"] != "direct");
            }
            "changed-archive" => changed["instances"][0]["sha256"] = json!("0".repeat(64)),
            "missing-evidence" => changed["instances"][0]["evidence"] = json!([]),
            _ => {
                for row in changed["instances"].as_array_mut().unwrap() {
                    if mutation == "failed-upgrade" && row["distribution"] == "npm" {
                        row["checks"]["upgrade_rollback"] = json!(false)
                    }
                    if mutation == "failed-direct" && row["distribution"] == "direct" {
                        row["checks"]["launch_cleanup"] = json!(false)
                    }
                }
            }
        }
        let mut input = input.clone();
        input["report"] = document(&f.dir, "changed-final-report.json", &changed);
        assert!(
            authorization::authorize(&f.dir.0, &input, &candidate).is_err(),
            "{mutation}"
        );
    }
    let args = vec![
        output.to_string_lossy().into_owned(),
        "--inputs".into(),
        f.dir
            .file("final-inputs.json", &json_bytes(&input))
            .unwrap()
            .to_string_lossy()
            .into_owned(),
    ];
    authorization::run(&args).unwrap();
    let npm = output.join("claude-autorouter-0.4.0.tgz");
    assert!(
        release::inspect_artifact(&npm, "v0.4.0", false)
            .unwrap()
            .native
    );
    assert!(
        release::inspect_artifact(&npm, "v0.4.0", true)
            .unwrap()
            .native
    );
    assert!(authorization::run(&args).is_err());
    let mut bad = auth;
    bad["release_index_sha256"] = json!("0".repeat(64));
    let bytes = json_bytes(&bad);
    std::fs::write(output.join("release-authorization.json"), &bytes).unwrap();
    std::fs::write(
        output.join("release-authorization.json.sha256"),
        format!("{}  release-authorization.json\n", digest(&bytes)),
    )
    .unwrap();
    assert!(verify_authorization(&npm).is_err());
}

// Independent contract names: preserve all named source materials selected by
// the native producer, rather than deriving the missing-set oracle from the
// admission helper being tested.
const NAMED_PUBLIC_FILES_CONTRACT: [&str; 22] = [
    "package.json",
    "README.md",
    "CONTRIBUTING.md",
    "LICENSE",
    ".env.example",
    "SECURITY.md",
    "SUPPORT.md",
    "CODE_OF_CONDUCT.md",
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

fn remove_named_public_file_and_its_declaration(files: &mut BTreeMap<String, Entry>, path: &str) {
    assert!(files.remove(path).is_some(), "missing fixture path {path}");
    let manifest = files.get_mut("build-manifest.json").unwrap();
    let mut build = json_read(&manifest.bytes).unwrap();
    let declarations = build["files"].as_array_mut().unwrap();
    let before = declarations.len();
    declarations.retain(|row| row["path"] != path);
    assert_eq!(declarations.len() + 1, before, "{path}");
    manifest.bytes = json_bytes(&build);
}

#[test]
fn native_named_public_set_rejects_every_self_consistent_missing_source_material() {
    let f = Fixture::new();
    let output = f.assembled();
    let (files, _) = archive::decode(&output["claude-autorouter-0.4.0.tgz"], archive::NATIVE)
        .unwrap()
        .into_parts();
    let manifest = json_read(&files["package.json"].bytes).unwrap();
    release::native_files(&files, &manifest).unwrap();

    // Retain the original LICENSE deletion without changing declarations as
    // a separate control: the stale declaration must still fail first.
    let mut unchanged_declaration = files.clone();
    assert!(unchanged_declaration.remove("LICENSE").is_some());
    assert_eq!(
        release::native_files(&unchanged_declaration, &manifest)
            .err()
            .as_deref(),
        Some("Declared release file missing")
    );

    let mut checked = 0;
    for path in NAMED_PUBLIC_FILES_CONTRACT {
        let mut changed = files.clone();
        remove_named_public_file_and_its_declaration(&mut changed, path);
        let expected = format!("Native runtime documentation or file missing: {path}");
        assert_eq!(
            release::native_files(&changed, &manifest).err().as_deref(),
            Some(expected.as_str()),
            "self-consistent omission {path}"
        );
        checked += 1;
    }
    assert_eq!(checked, 22);
    release::native_files(&files, &manifest).unwrap();
    eprintln!(
        "native_named_public_set={{\"missing_cases\":22,\"original_license_case\":true,\"baseline_before_after\":true}}"
    );
}

#[test]
fn direct_named_public_set_rejects_every_self_consistent_missing_source_material() {
    let f = Fixture::new();
    let output = f.assembled();
    let name = "claude-autorouter-0.4.0-aarch64-apple-darwin.tar.gz";
    direct::verify_bytes(name, &output[name]).unwrap();
    let (files, _) = archive::decode_root(&output[name], PACKAGE, archive::NATIVE)
        .unwrap()
        .into_parts();

    // The direct verifier also retains its earlier declaration-first guard.
    let mut unchanged_declaration = files.clone();
    assert!(unchanged_declaration.remove("LICENSE").is_some());
    let bytes = archive::encode_root(PACKAGE, &unchanged_declaration, archive::NATIVE).unwrap();
    assert_eq!(
        direct::verify_bytes(name, &bytes).err().as_deref(),
        Some("Missing direct file")
    );

    let mut checked = 0;
    for path in NAMED_PUBLIC_FILES_CONTRACT
        .into_iter()
        .filter(|path| *path != "package.json")
    {
        let mut changed = files.clone();
        remove_named_public_file_and_its_declaration(&mut changed, path);
        let bytes = archive::encode_root(PACKAGE, &changed, archive::NATIVE).unwrap();
        assert_eq!(
            direct::verify_bytes(name, &bytes).err().as_deref(),
            Some("Direct runtime documentation baseline missing"),
            "self-consistent omission {path}"
        );
        checked += 1;
    }
    assert_eq!(checked, 21);
    direct::verify_bytes(name, &output[name]).unwrap();
    eprintln!(
        "direct_named_public_set={{\"missing_cases\":21,\"original_license_case\":true,\"baseline_before_after\":true}}"
    );
}

#[test]
fn shared_cli_fixture_preserves_complete_production_assembly_and_authorization() {
    assert_eq!(cli_fixture::PACKAGE, PACKAGE);
    assert_eq!(cli_fixture::REPOSITORY, release::REPOSITORY);
    assert_eq!(cli_fixture::REGISTRY, release::REGISTRY);
    assert_eq!(cli_fixture::ROOT_FILES, release::ROOT_FILES);
    assert_eq!(cli_fixture::DOCS, release::DOCS);
    assert_eq!(cli_fixture::GATES, GATES);
    for gate in GATES {
        assert_eq!(cli_fixture::checks(gate), checks(gate));
    }
    let fixture = Fixture::new();
    let raw = cli_fixture::MaterialFixture::new(fixture.source.clone());
    // The explicit-byte constructor must retain the complete old header case,
    // including both independent archive encodings and final metadata.
    let explicit = cli_fixture::MaterialFixture::with_binary(
        fixture.source.clone(),
        cli_fixture::TARGET,
        raw.materials["binary"].clone(),
        crate::package::binary::inspect(cli_fixture::TARGET, &raw.materials["binary"]).unwrap(),
    );
    assert_eq!(explicit.materials, raw.materials);
    assert_eq!(explicit.inputs, raw.inputs);
    assert_eq!(explicit.baseline, raw.baseline);
    assert_eq!(explicit.public, raw.public);
    assert_eq!(explicit.public_sha, raw.public_sha);
    let old_archives = raw.assembled();
    // Frozen actual prior-main observations fix the original header identity;
    // this is independent of comparing two newly refactored constructors.
    assert_eq!(digest(&old_archives["claude-autorouter-0.4.0.tgz"]), "2a55bca6c4516ef98dfd56e9a0744682f171cdb4590d561dacb485385909cf81");
    assert_eq!(digest(&old_archives["claude-autorouter-0.4.0-aarch64-apple-darwin.tar.gz"]), "9e66cc15f3aaefc8d1eb6dba2bce5b1e6821810579a8201bb8747a93294cc371");
    assert_eq!(digest(&old_archives["release-index.json"]), "4d4c6c459875c4b0cae9e297b972dcc6af9b793245c54993e41774aa3be80bcd");
    assert_eq!(explicit.assembled(), old_archives);
    let old_final = raw.final_materials(&old_archives);
    assert_eq!(explicit.final_materials(&old_archives), old_final);
    assert_eq!(
        explicit.expected_authorization(&old_archives, &old_final),
        raw.expected_authorization(&old_archives, &old_final)
    );
    assert_eq!(raw.inputs, fixture.inputs);
    assert_eq!(raw.baseline, fixture.baseline);
    assert_eq!(raw.public_sha, fixture.public_sha);
    for (name, bytes) in &raw.materials {
        assert_eq!(std::fs::read(fixture.dir.0.join(name)).unwrap(), *bytes);
    }
    let expected = raw.assembled();
    assert_eq!(
        fixture.assembled(),
        expected,
        "Every npm/direct/index/sidecar byte must stay identical"
    );
    let output = fixture.dir.0.join("complete-cli-fixture");
    write_output(&output, &expected).unwrap();
    let final_materials = raw.final_materials(&expected);
    for (name, bytes) in &final_materials {
        fixture.dir.file(name, bytes).unwrap();
    }
    let candidate = authorization::candidate(&output).unwrap();
    let input = json_read(&final_materials["final-inputs.json"]).unwrap();
    assert_eq!(
        authorization::authorize(&fixture.dir.0, &input, &candidate).unwrap(),
        raw.expected_authorization(&expected, &final_materials)
    );
}
