//! Synthetic declarations and executable headers test rejection, never qualification.
use super::*;
use crate::tool_process::Scratch;
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
        let dir = Scratch::new("release-pack-synthetic").unwrap();
        let source = json!({"commit":"a".repeat(40),"dirty":false,"cargo_lock_sha256":"b".repeat(64),"provenance":"ci-source-build"});
        let target = "aarch64-apple-darwin";
        let baseline = json!({"baseline":{"package":"claude-autorouter@0.5.2","node":">=22","cpu_restriction":null},"targets":[{"target":target}],"unresolved_baseline_architectures":["synthetic unresolved platform"],"artifact_caps":{"compressed_bytes":archive::MAX_COMPRESSED,"expanded_tar_bytes":archive::MAX_NATIVE_EXPANDED,"file_bytes":archive::MAX_FILE,"entries":archive::NATIVE.entries}});
        let evidence = material(
            &dir,
            "evidence.json",
            b"synthetic fixture only, no qualification claim",
        );
        let platforms = json!({"schema_version":1,"status":"release_matrix_approved","baseline":baseline["baseline"],"artifact_caps":baseline["artifact_caps"],"unresolved_baseline_architectures":[],"targets":[{"target":target,"qualification":"passed"}],"baseline_resolutions":[{"architecture":"synthetic unresolved platform","result":"baseline_unavailable","reason":"synthetic unavailable baseline fixture","evidence_sha256":evidence["sha256"],"review_reference":"synthetic:test"}]});
        let platform_descriptor = document(&dir, "platforms.json", &platforms);
        let licenses = material(
            &dir,
            "licenses.txt",
            b"Synthetic license fixture, not a real license inventory",
        );
        let mut public = BTreeMap::new();
        for path in release::ROOT_FILES.iter().chain(release::DOCS) {
            insert(
                &mut public,
                path,
                b"synthetic public documentation\n".to_vec(),
                0o644,
            );
        }
        insert(
            &mut public,
            "package.json",
            json_bytes(
                &json!({"name":PACKAGE,"version":"0.4.0","repository":{"url":format!("git+https://github.com/{}.git",release::REPOSITORY)},"publishConfig":{"access":"public","registry":format!("{}/",release::REGISTRY)},"bin":{PACKAGE:"bin/autorouter.mjs"},"engines":{"node":">=22"},"scripts":{"test":"node test.mjs"},"devDependencies":{"typescript":"synthetic"}}),
            ),
            0o644,
        );
        let public_sha = digest(&json_bytes(&declarations(&public)));
        let mut qualification = json!({});
        let mut report_hashes = json!({});
        for gate in GATES {
            let mut flags = json!({});
            for key in checks(gate) {
                flags[key] = json!(true);
            }
            let report = json!({"schema_version":1,"kind":"native_release_qualification","gate":gate,"source_commit":source["commit"],"cargo_lock_sha256":source["cargo_lock_sha256"],"passed":true,"complete":true,"checks":flags,"evidence":[evidence],"platforms_sha256":platform_descriptor["sha256"],"licenses_sha256":licenses["sha256"],"targets":[target]});
            let desc = document(&dir, &format!("gate-{gate}.json"), &report);
            report_hashes[gate] = desc["sha256"].clone();
            qualification[gate] = desc;
        }
        let mut binary = vec![0u8; 32];
        binary[..4].copy_from_slice(&[0xcf, 0xfa, 0xed, 0xfe]);
        binary[4..8].copy_from_slice(&0x0100000cu32.to_le_bytes());
        binary[12..16].copy_from_slice(&2u32.to_le_bytes());
        let binary_desc = material(&dir, "binary", &binary);
        let provenance = json!({"schema_version":1,"kind":"native_binary_provenance","provenance":"ci-source-build","source_commit":source["commit"],"cargo_lock_sha256":source["cargo_lock_sha256"],"target":target,"binary_sha256":binary_desc["sha256"],"repository":release::REPOSITORY,"profile":"release","portable_cpu":true,"rustc":"synthetic rustc","workflow_run":"synthetic:123","rustflags":[]});
        let provenance_desc = document(&dir, "provenance.json", &provenance);
        for gate in GATES {
            let name = format!("gate-{gate}.json");
            let mut report = json_read(&std::fs::read(dir.0.join(&name)).unwrap()).unwrap();
            report["artifacts"] = json!({target:{"binary_sha256":binary_desc["sha256"],"provenance_sha256":provenance_desc["sha256"]}});
            let desc = document(&dir, &name, &report);
            report_hashes[gate] = desc["sha256"].clone();
            qualification[gate] = desc;
        }
        let approval = json!({"schema_version":1,"kind":"native_release_approval","approved":true,"source_commit":source["commit"],"cargo_lock_sha256":source["cargo_lock_sha256"],"version":"0.4.0","platforms_sha256":platform_descriptor["sha256"],"licenses_sha256":licenses["sha256"],"public_files_sha256":public_sha,"qualification":report_hashes,"artifacts":{target:{"binary_sha256":binary_desc["sha256"],"provenance_sha256":provenance_desc["sha256"]}},"reviewer":"synthetic fixture","review_reference":"synthetic:test","reviewed":{"baseline_platform_scope":true,"runtime_documentation":true,"live_canaries":true,"license_review":true}});
        let approval_desc = document(&dir, "approval.json", &approval);
        let inputs = json!({"schema_version":1,"kind":"native_release_inputs","platforms":platform_descriptor,"licenses":licenses,"qualification":qualification,"approval":approval_desc,"artifacts":[{"target":target,"binary":binary_desc,"provenance":provenance_desc}]});
        Self {
            dir,
            inputs,
            source,
            baseline,
            public,
            public_sha,
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
