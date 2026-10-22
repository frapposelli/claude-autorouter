//! Shared pure synthetic fixture material, never real release qualification.
//! The default header fixture is unchanged; explicit host bytes enable installed-native controls.
use serde_json::{Value, json};
use sha2::{Digest, Sha256, Sha512};
use std::collections::BTreeMap;
use std::io::Write;
pub const PACKAGE: &str = "claude-autorouter";
pub const REPOSITORY: &str = "frapposelli/claude-autorouter";
pub const REGISTRY: &str = "https://registry.npmjs.org";
pub const TARGET: &str = "aarch64-apple-darwin";
pub const FINAL_CHECKS: &[&str] = &[
    "byte_integrity",
    "help_version",
    "no_node_runtime",
    "launch_cleanup",
    "terminal_signals",
    "tls_custom_ca",
    "offline_install_no_scripts",
    "upgrade_rollback",
];
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub bytes: Vec<u8>,
    pub mode: u32,
}
pub fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
pub fn json_bytes(value: &Value) -> Vec<u8> {
    let mut bytes = serde_json::to_vec_pretty(value).unwrap();
    bytes.push(b'\n');
    bytes
}
fn json_read(bytes: &[u8]) -> Result<Value, serde_json::Error> {
    serde_json::from_slice(bytes)
}
fn material(files: &mut BTreeMap<String, Vec<u8>>, name: &str, bytes: &[u8]) -> Value {
    files.insert(name.into(), bytes.to_vec());
    json!({"path":name,"sha256":digest(bytes)})
}
fn document(files: &mut BTreeMap<String, Vec<u8>>, name: &str, value: &Value) -> Value {
    material(files, name, &json_bytes(value))
}
fn insert(files: &mut BTreeMap<String, Entry>, path: &str, bytes: Vec<u8>, mode: u32) {
    files.insert(path.into(), Entry { bytes, mode });
}
pub fn declarations(files: &BTreeMap<String, Entry>) -> Value {
    Value::Array(files.iter().map(|(path, entry)| json!({"path":path,"bytes":entry.bytes.len(),"mode":entry.mode,"sha256":digest(&entry.bytes)})).collect())
}
pub const DOCS: &[&str] = &[
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
pub const ROOT_FILES: &[&str] = &[
    "package.json",
    "README.md",
    "CONTRIBUTING.md",
    "LICENSE",
    ".env.example",
    "SECURITY.md",
    "SUPPORT.md",
    "CODE_OF_CONDUCT.md",
];
pub const GATES: &[&str] = &[
    "compatibility",
    "performance",
    "platforms",
    "licenses",
    "installed_lifecycle",
];
pub fn checks(gate: &str) -> &'static [&'static str] {
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
pub struct MaterialFixture {
    pub materials: BTreeMap<String, Vec<u8>>,
    pub inputs: Value,
    pub source: Value,
    pub baseline: Value,
    pub public: BTreeMap<String, Entry>,
    pub public_sha: String,
    target: String,
    inspection: Value,
    fixture_cap: usize,
}
impl MaterialFixture {
    pub fn new(source: Value) -> Self {
        let mut binary = vec![0u8; 32];
        binary[..4].copy_from_slice(&[0xcf, 0xfa, 0xed, 0xfe]);
        binary[4..8].copy_from_slice(&0x0100000cu32.to_le_bytes());
        binary[12..16].copy_from_slice(&2u32.to_le_bytes());
        Self::build(
            source,
            TARGET,
            binary,
            json!({"format":"Mach-O","target":TARGET,"minimum_macos":null,"dynamic_libraries":[],"execution_qualification":"pending"}),
            1024 * 1024,
        )
    }
    pub fn with_binary(source: Value, target: &str, binary: Vec<u8>, inspection: Value) -> Self {
        assert!(!binary.is_empty() && binary.len() <= 16 * 1024 * 1024);
        assert_eq!(inspection["target"], target);
        Self::build(source, target, binary, inspection, 32 * 1024 * 1024)
    }
    fn build(
        source: Value,
        target: &str,
        binary: Vec<u8>,
        inspection: Value,
        fixture_cap: usize,
    ) -> Self {
        let mut materials = BTreeMap::new();
        let baseline = json!({"baseline":{"package":"claude-autorouter@0.5.2","node":">=22","cpu_restriction":null},"targets":[{"target":target}],"unresolved_baseline_architectures":["synthetic unresolved platform"],"artifact_caps":{"compressed_bytes":32 * 1024 * 1024,"expanded_tar_bytes":64 * 1024 * 1024,"file_bytes":32 * 1024 * 1024,"entries":256}});
        let evidence = material(
            &mut materials,
            "evidence.json",
            b"synthetic fixture only, no qualification claim",
        );
        let platforms = json!({"schema_version":1,"status":"release_matrix_approved","baseline":baseline["baseline"],"artifact_caps":baseline["artifact_caps"],"unresolved_baseline_architectures":[],"targets":[{"target":target,"qualification":"passed"}],"baseline_resolutions":[{"architecture":"synthetic unresolved platform","result":"baseline_unavailable","reason":"synthetic unavailable baseline fixture","evidence_sha256":evidence["sha256"],"review_reference":"synthetic:test"}]});
        let platform_descriptor = document(&mut materials, "platforms.json", &platforms);
        let licenses = material(
            &mut materials,
            "licenses.txt",
            b"Synthetic license fixture, not a real license inventory",
        );
        let mut public = BTreeMap::new();
        for path in ROOT_FILES.iter().chain(DOCS) {
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
                &json!({"name":PACKAGE,"version":"0.4.0","repository":{"url":format!("git+https://github.com/{}.git",REPOSITORY)},"publishConfig":{"access":"public","registry":format!("{}/",REGISTRY)},"bin":{PACKAGE:"bin/autorouter.mjs"},"engines":{"node":">=22"},"scripts":{"test":"node test.mjs"},"devDependencies":{"typescript":"synthetic"}}),
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
            let desc = document(&mut materials, &format!("gate-{gate}.json"), &report);
            report_hashes[gate] = desc["sha256"].clone();
            qualification[gate] = desc;
        }
        let binary_desc = material(&mut materials, "binary", &binary);
        let provenance = json!({"schema_version":1,"kind":"native_binary_provenance","provenance":"ci-source-build","source_commit":source["commit"],"cargo_lock_sha256":source["cargo_lock_sha256"],"target":target,"binary_sha256":binary_desc["sha256"],"repository":REPOSITORY,"profile":"release","portable_cpu":true,"rustc":"synthetic rustc","workflow_run":"synthetic:123","rustflags":[]});
        let provenance_desc = document(&mut materials, "provenance.json", &provenance);
        for gate in GATES {
            let name = format!("gate-{gate}.json");
            let mut report: Value = serde_json::from_slice(&materials[&name]).unwrap();
            report["artifacts"] = json!({target:{"binary_sha256":binary_desc["sha256"],"provenance_sha256":provenance_desc["sha256"]}});
            let desc = document(&mut materials, &name, &report);
            report_hashes[gate] = desc["sha256"].clone();
            qualification[gate] = desc;
        }
        let approval = json!({"schema_version":1,"kind":"native_release_approval","approved":true,"source_commit":source["commit"],"cargo_lock_sha256":source["cargo_lock_sha256"],"version":"0.4.0","platforms_sha256":platform_descriptor["sha256"],"licenses_sha256":licenses["sha256"],"public_files_sha256":public_sha,"qualification":report_hashes,"artifacts":{target:{"binary_sha256":binary_desc["sha256"],"provenance_sha256":provenance_desc["sha256"]}},"reviewer":"synthetic fixture","review_reference":"synthetic:test","reviewed":{"baseline_platform_scope":true,"runtime_documentation":true,"live_canaries":true,"license_review":true}});
        let approval_desc = document(&mut materials, "approval.json", &approval);
        let inputs = json!({"schema_version":1,"kind":"native_release_inputs","platforms":platform_descriptor,"licenses":licenses,"qualification":qualification,"approval":approval_desc,"artifacts":[{"target":target,"binary":binary_desc,"provenance":provenance_desc}]});
        Self {
            materials,
            inputs,
            source,
            baseline,
            public,
            public_sha,
            target: target.into(),
            inspection,
            fixture_cap,
        }
    }
    /// Independent fixture construction. A unit control compares every output
    /// byte to the unchanged production qualify/assemble path before CLI reuse.
    pub fn assembled(&self) -> BTreeMap<String, Vec<u8>> {
        let mut files = self.public.clone();
        let mut manifest = json_read(&files["package.json"].bytes).unwrap();
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
            manifest.as_object_mut().unwrap().remove(key);
        }
        manifest["bin"] = json!({PACKAGE:"bin/autorouter"});
        manifest["files"] = json!(
            ROOT_FILES
                .iter()
                .chain(DOCS)
                .copied()
                .chain([
                    "bin/autorouter",
                    "native",
                    "build-manifest.json",
                    "platforms.json",
                    "THIRD-PARTY-LICENSES.txt"
                ])
                .collect::<Vec<_>>()
        );
        insert(&mut files, "package.json", json_bytes(&manifest), 0o644);
        insert(
            &mut files,
            "platforms.json",
            self.materials["platforms.json"].clone(),
            0o644,
        );
        insert(
            &mut files,
            "THIRD-PARTY-LICENSES.txt",
            self.materials["licenses.txt"].clone(),
            0o644,
        );
        insert(
            &mut files,
            "bin/autorouter",
            include_bytes!("../../distribution/autorouter.sh").to_vec(),
            0o755,
        );
        let target = &self.target;
        let binary_path = format!("native/{target}/{PACKAGE}");
        insert(
            &mut files,
            &binary_path,
            self.materials["binary"].clone(),
            0o755,
        );
        let qualification: serde_json::Map<String, Value> = GATES.iter().map(|gate| ((*gate).to_owned(), json!({"passed":true,"report_sha256":digest(&self.materials[&format!("gate-{gate}.json")]),"source_commit":self.source["commit"]}))).collect();
        let approval = json!({"approved":true,"record_sha256":digest(&self.materials["approval.json"]),"source_commit":self.source["commit"]});
        let artifact = json!({"target":target,"path":binary_path,"sha256":digest(&self.materials["binary"]),"source_commit":self.source["commit"],"cargo_lock_sha256":self.source["cargo_lock_sha256"],"provenance_sha256":digest(&self.materials["provenance.json"]),"inspection":self.inspection});
        let build = json!({"schema_version":2,"kind":"native_npm_release","release_approved":false,"qualification_approved":true,"version":"0.4.0","source":self.source,"qualification":qualification,"approval":approval,"files":declarations(&files),"artifacts":[artifact]});
        insert(&mut files, "build-manifest.json", json_bytes(&build), 0o644);
        let npm_name = format!("{PACKAGE}-0.4.0.tgz");
        let npm = encode_fixture("package", &files, self.fixture_cap);
        let npm_hash = digest(&npm);
        let mut direct = BTreeMap::new();
        for (path, entry) in &files {
            if (ROOT_FILES.contains(&path.as_str()) && path != "package.json")
                || DOCS.contains(&path.as_str())
                || ["platforms.json", "THIRD-PARTY-LICENSES.txt"].contains(&path.as_str())
            {
                direct.insert(path.clone(), entry.clone());
            }
        }
        direct.insert("bin/claude-autorouter".into(), files[&binary_path].clone());
        let mut direct_artifact = artifact;
        direct_artifact["path"] = json!("bin/claude-autorouter");
        let direct_manifest = json!({"schema_version":2,"kind":"native_direct_release","release_approved":false,"qualification_approved":true,"version":"0.4.0","target":target,"source":self.source,"qualification":build["qualification"],"approval":build["approval"],"npm":{"filename":npm_name,"sha256":npm_hash},"files":declarations(&direct),"artifact":direct_artifact});
        insert(
            &mut direct,
            "build-manifest.json",
            json_bytes(&direct_manifest),
            0o644,
        );
        let mut output = BTreeMap::from([
            (npm_name, npm),
            (
                format!("{PACKAGE}-0.4.0-{target}.tar.gz"),
                encode_fixture(PACKAGE, &direct, self.fixture_cap),
            ),
        ]);
        let index = json!({"schema_version":2,"kind":"native_release_artifacts","release_approved":false,"final_authorization":"required","version":"0.4.0","source":self.source,"qualification":build["qualification"],"approval":build["approval"],"archives":output.iter().map(|(name, bytes)| json!({"filename":name,"sha256":digest(bytes),"bytes":bytes.len(),"integrity":integrity(bytes)})).collect::<Vec<_>>()});
        output.insert("release-index.json".into(), json_bytes(&index));
        let checksums: Vec<_> = output
            .iter()
            .map(|(name, bytes)| {
                (
                    format!("{name}.sha256"),
                    format!("{}  {name}\n", digest(bytes)).into_bytes(),
                )
            })
            .collect();
        output.extend(checksums);
        output
    }
    /// Same synthetic final report/review facts as archive_contracts::authorize.
    /// The real public authorizer must validate these against the entire set.
    pub fn final_materials(
        &self,
        archives: &BTreeMap<String, Vec<u8>>,
    ) -> BTreeMap<String, Vec<u8>> {
        let mut materials = BTreeMap::new();
        let evidence = material(
            &mut materials,
            "installed-contract-evidence.json",
            b"synthetic declarations only, not release qualification",
        );
        let checks: serde_json::Map<String, Value> = FINAL_CHECKS
            .iter()
            .map(|key| ((*key).into(), json!(true)))
            .collect();
        let target = &self.target;
        let instances: Vec<_> = archives.iter().filter(|(name, _)| name.ends_with(".tgz") || name.ends_with(".tar.gz")).map(|(name, bytes)| json!({"filename":name,"target":target,"distribution":if name.ends_with(".tgz") {"npm"} else {"direct"},"sha256":digest(bytes),"passed":true,"checks":checks,"evidence":[evidence]})).collect();
        let index_hash = digest(&archives["release-index.json"]);
        let report = json!({"schema_version":1,"kind":"native_release_final_qualification","passed":true,"complete":true,"source":self.source,"release_index_sha256":index_hash,"instances":instances});
        let report_desc = document(&mut materials, "contract-final-report.json", &report);
        let approval = json!({"schema_version":1,"kind":"native_release_final_approval","approved":true,"source":self.source,"release_index_sha256":index_hash,"report_sha256":report_desc["sha256"],"reviewer":"synthetic fixture","review_reference":"synthetic:contract"});
        let approval_desc = document(&mut materials, "contract-final-approval.json", &approval);
        document(
            &mut materials,
            "final-inputs.json",
            &json!({"schema_version":1,"kind":"native_release_final_inputs","report":report_desc,"approval":approval_desc}),
        );
        materials
    }
    pub fn expected_authorization(
        &self,
        archives: &BTreeMap<String, Vec<u8>>,
        materials: &BTreeMap<String, Vec<u8>>,
    ) -> Value {
        let report: Value = json_read(&materials["contract-final-report.json"]).unwrap();
        let instances: Vec<_> = report["instances"].as_array().unwrap().iter().map(|row| json!({"filename":row["filename"],"target":row["target"],"distribution":row["distribution"],"sha256":row["sha256"],"passed":true,"checks":row["checks"],"evidence_sha256":[digest(&materials["installed-contract-evidence.json"])]})).collect();
        json!({"schema_version":2,"kind":"native_release_authorization","approved":true,"version":"0.4.0","source":self.source,"release_index_sha256":digest(&archives["release-index.json"]),"report_sha256":digest(&materials["contract-final-report.json"]),"approval_sha256":digest(&materials["contract-final-approval.json"]),"instances":instances})
    }
}
pub fn integrity(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let hash = Sha512::digest(bytes);
    let mut text = String::from("sha512-");
    for chunk in hash.chunks(3) {
        let a = chunk[0];
        let b = chunk.get(1).copied().unwrap_or(0);
        let c = chunk.get(2).copied().unwrap_or(0);
        text.push(TABLE[(a >> 2) as usize] as char);
        text.push(TABLE[(((a & 3) << 4) | (b >> 4)) as usize] as char);
        text.push(if chunk.len() > 1 {
            TABLE[(((b & 15) << 2) | (c >> 6)) as usize] as char
        } else {
            '='
        });
        text.push(if chunk.len() > 2 {
            TABLE[(c & 63) as usize] as char
        } else {
            '='
        });
    }
    text
}
/// Small fixed-fixture encoder, not an alternative product archive policy.
/// Production unit equality covers every byte including the deterministic gzip header.
fn encode_fixture(root: &str, files: &BTreeMap<String, Entry>, cap: usize) -> Vec<u8> {
    assert!(!files.is_empty() && files.len() <= 64);
    let mut writer = flate2::GzBuilder::new()
        .mtime(0)
        .write(Vec::new(), flate2::Compression::default());
    let mut expanded = 1024;
    for (path, entry) in files {
        let full = format!("{root}/{path}");
        assert!(full.len() <= 100 && !path.contains("..") && !path.starts_with('/'));
        assert!(matches!(entry.mode, 0o644 | 0o755));
        let padded = entry.bytes.len().div_ceil(512) * 512;
        expanded += 512 + padded;
        assert!(expanded <= cap);
        let mut header = [0_u8; 512];
        header[..full.len()].copy_from_slice(full.as_bytes());
        for (start, len, value) in [
            (100, 8, entry.mode as u64),
            (108, 8, 0),
            (116, 8, 0),
            (124, 12, entry.bytes.len() as u64),
            (136, 12, 0),
        ] {
            let field = format!("{:0width$o}\0", value, width = len - 1);
            assert_eq!(field.len(), len);
            header[start..start + len].copy_from_slice(field.as_bytes());
        }
        header[156] = b'0';
        header[257..263].copy_from_slice(b"ustar\0");
        header[263..265].copy_from_slice(b"00");
        header[148..156].fill(b' ');
        let checksum: u64 = header.iter().map(|byte| *byte as u64).sum();
        header[148..156].copy_from_slice(format!("{checksum:06o}\0 ").as_bytes());
        writer.write_all(&header).unwrap();
        writer.write_all(&entry.bytes).unwrap();
        writer
            .write_all(&[0; 512][..padded - entry.bytes.len()])
            .unwrap();
    }
    writer.write_all(&[0; 1024]).unwrap();
    let bytes = writer.finish().unwrap();
    assert!(bytes.len() <= cap);
    bytes
}
