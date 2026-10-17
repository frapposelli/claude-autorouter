//! Inspect actual ordinary release Cargo artifacts, never dev-unified metadata.
use crate::audit_support::{digest, regular};
use serde_json::{Value, json};
use std::path::{Component, Path, PathBuf};
const REPORT_LIMIT: u64 = 64 * 1024 * 1024;
const RECORD_LIMIT: usize = 100_000;
const LINE_LIMIT: usize = 2 * 1024 * 1024;

fn manifest(root: &Path, name: &str) -> PathBuf {
    root.join("rust").join(name).join("Cargo.toml")
}
fn includes(value: &Value, name: &str) -> bool {
    value
        .as_array()
        .is_some_and(|items| items.iter().any(|item| item == name))
}
fn release_profile(row: &Value) -> bool {
    row["profile"]["test"] == false
        && row["profile"]["opt_level"] == "3"
        && row["profile"]["debug_assertions"] == false
}
fn library(row: &Value, path: &Path, name: &str, version: &str) -> Result<(), String> {
    let url = url::Url::from_directory_path(path.parent().ok_or("Invalid library manifest path")?)
        .map_err(|_| "Invalid vendored package URL")?;
    let prefix = format!("path+{}#", url.as_str().trim_end_matches('/'));
    let id = row["package_id"]
        .as_str()
        .ok_or("Invalid Cargo package identity")?;
    if row["manifest_path"].as_str() != path.to_str()
        || ![
            format!("{prefix}{version}"),
            format!("{prefix}{name}@{version}"),
        ]
        .contains(&id.to_owned())
    {
        return Err("Cargo artifact is not the exact reviewed library package".into());
    }
    let features = row["features"]
        .as_array()
        .filter(|items| items.iter().all(Value::is_string))
        .ok_or("Invalid Cargo library feature list")?;
    if features.iter().any(|name| {
        name == "development-probes"
            || name == "node-http1-body-handoff"
            || name == "node-http1-request-lease"
            || name == "node-http1-raw-pool"
    }) {
        return Err(
            "Shipping build enables an experimental transport or development feature".into(),
        );
    }
    if !release_profile(row) {
        return Err("Library artifact is not an ordinary release build".into());
    }
    Ok(())
}

pub(crate) fn inspect(bytes: &[u8], root: &Path) -> Result<Value, String> {
    if bytes.len() as u64 > REPORT_LIMIT {
        return Err("Cargo report exceeds the byte limit".into());
    }
    let text = std::str::from_utf8(bytes).map_err(|_| "Invalid Cargo report UTF-8")?;
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    if text.split('\n').count() > RECORD_LIMIT {
        return Err("Cargo report exceeds the record limit".into());
    }
    let hyper_manifest = manifest(root, "vendor/hyper");
    let util_manifest = manifest(root, "vendor/hyper-util");
    let cli_manifest = manifest(root, "crates/claude-autorouter");
    let runtime_manifest = manifest(root, "crates/autorouter-runtime");
    let mut hyper = Vec::new();
    let mut util = Vec::new();
    let mut binaries = Vec::new();
    let mut runtime = Vec::new();
    let mut finished = false;
    for line in text.split('\n') {
        if line.trim().is_empty() {
            continue;
        }
        if line.len() > LINE_LIMIT {
            return Err("Cargo record exceeds the byte limit".into());
        }
        if finished {
            return Err("Cargo records follow build-finished".into());
        }
        let row: Value = serde_json::from_str(line).map_err(|_| "Invalid Cargo record JSON")?;
        if !row.is_object() {
            return Err("Invalid Cargo record".into());
        }
        if row["reason"] == "build-finished" {
            if row["success"] != true {
                return Err("Cargo build did not succeed".into());
            }
            finished = true;
        }
        if row["reason"] == "compiler-message" && row["message"]["level"] == "error" {
            return Err("Cargo emitted an error".into());
        }
        if row["reason"] != "compiler-artifact" {
            continue;
        }
        if row["target"]["name"] == "hyper" && includes(&row["target"]["kind"], "lib") {
            library(&row, &hyper_manifest, "hyper", "1.12.0")?;
            hyper.push(row);
        } else if row["target"]["name"] == "hyper_util" && includes(&row["target"]["kind"], "lib") {
            library(&row, &util_manifest, "hyper-util", "0.1.21")?;
            util.push(row);
        } else if row["target"]["name"] == "autorouter_runtime"
            && includes(&row["target"]["kind"], "lib")
        {
            library(
                &row,
                &runtime_manifest,
                "autorouter-runtime",
                env!("CARGO_PKG_VERSION"),
            )?;
            runtime.push(row);
        } else if row["target"]["name"] == "claude-autorouter"
            && includes(&row["target"]["kind"], "bin")
        {
            if row["manifest_path"].as_str() != cli_manifest.to_str() || !release_profile(&row) {
                return Err("CLI artifact is not an ordinary release build".into());
            }
            let executable = row["executable"]
                .as_str()
                .ok_or("Missing release executable artifact")?;
            let executable_path = Path::new(executable);
            if !includes(&row["filenames"], executable)
                || !executable_path
                    .components()
                    .any(|part| part == Component::Normal(std::ffi::OsStr::new("release")))
                || !executable_path.file_name().is_some_and(|name| {
                    name == "claude-autorouter" || name == "claude-autorouter.exe"
                })
            {
                return Err("Invalid release executable artifact".into());
            }
            binaries.push(row);
        }
    }
    if !finished || hyper.len() != 1 || util.len() != 1 || runtime.len() != 1 || binaries.len() != 1
    {
        return Err("Cargo report lacks a unique successful shipping build".into());
    }
    Ok(
        json!({"library":util.remove(0),"hyper_library":hyper.remove(0),"runtime_library":runtime.remove(0),"binary":binaries.remove(0)}),
    )
}

fn fixture(root: &Path) -> Vec<Value> {
    let profile = json!({"test":false,"opt_level":"3","debug_assertions":false});
    let mut rows = Vec::new();
    for (source, name, version, features) in [
        (
            "vendor/hyper-util",
            "hyper_util",
            "0.1.21",
            vec!["client", "client-legacy", "http1", "tokio"],
        ),
        (
            "vendor/hyper",
            "hyper",
            "1.12.0",
            vec!["client", "http1", "server", "node-http1-compat"],
        ),
    ] {
        let path = manifest(root, source);
        let url = url::Url::from_directory_path(path.parent().unwrap()).unwrap();
        rows.push(json!({"reason":"compiler-artifact","package_id":format!("path+{}#{version}",url.as_str().trim_end_matches('/')),"manifest_path":path,"target":{"name":name,"kind":["lib"]},"features":features,"profile":profile}));
    }
    let executable = root.join("rust/target/release/claude-autorouter");
    rows.push(json!({"reason":"compiler-artifact","manifest_path":manifest(root,"crates/claude-autorouter"),"target":{"name":"claude-autorouter","kind":["bin"]},"profile":profile,"executable":executable,"filenames":[executable]}));
    let runtime_path = manifest(root, "crates/autorouter-runtime");
    let runtime_url = url::Url::from_directory_path(runtime_path.parent().unwrap()).unwrap();
    rows.push(json!({"reason":"compiler-artifact","package_id":format!("path+{}#{}",runtime_url.as_str().trim_end_matches('/'), env!("CARGO_PKG_VERSION")),"manifest_path":runtime_path,"target":{"name":"autorouter_runtime","kind":["lib"]},"features":[],"profile":profile}));
    rows.push(json!({"reason":"build-finished","success":true}));
    rows
}
fn encode(rows: &[Value]) -> Vec<u8> {
    rows.iter()
        .map(|row| serde_json::to_string(row).unwrap())
        .collect::<Vec<_>>()
        .join("\n")
        .into_bytes()
}

fn self_test(root: &Path) -> Result<usize, String> {
    let valid = fixture(root);
    inspect(&encode(&valid), root)?;
    let mut controls = Vec::new();
    for feature in ["node-http1-request-lease", "node-http1-raw-pool"] {
        let mut changed = valid.clone();
        changed[0]["features"]
            .as_array_mut()
            .unwrap()
            .push(json!(feature));
        controls.push(encode(&changed));
    }
    let mut changed = valid.clone();
    changed[1]["features"]
        .as_array_mut()
        .unwrap()
        .push(json!("node-http1-body-handoff"));
    controls.push(encode(&changed));
    for index in 0..valid.len() {
        let mut changed = valid.clone();
        changed.remove(index);
        controls.push(encode(&changed));
    }
    for index in 0..4 {
        let mut changed = valid.clone();
        changed.insert(0, valid[index].clone());
        controls.push(encode(&changed));
    }
    for index in [0, 1, 3] {
        for (field, value) in [
            ("manifest_path", json!("/different/Cargo.toml")),
            (
                "package_id",
                json!(format!(
                    "{}-wrong",
                    valid[index]["package_id"].as_str().unwrap()
                )),
            ),
            ("features", Value::Null),
            ("features", json!([123])),
        ] {
            let mut changed = valid.clone();
            changed[index][field] = value;
            controls.push(encode(&changed));
        }
        for (field, value) in [
            ("test", json!(true)),
            ("opt_level", json!("0")),
            ("debug_assertions", json!(true)),
        ] {
            let mut changed = valid.clone();
            changed[index]["profile"][field] = value;
            controls.push(encode(&changed));
        }
    }
    controls.push(encode(&[]));
    let mut changed = valid.clone();
    changed[4]["success"] = json!(false);
    controls.push(encode(&changed));
    let mut changed = valid.clone();
    changed.push(valid[4].clone());
    controls.push(encode(&changed));
    let mut changed = valid.clone();
    changed[2]["profile"]["test"] = json!(true);
    controls.push(encode(&changed));
    let mut changed = valid.clone();
    changed[2]["executable"] = json!("/tmp/debug/claude-autorouter");
    controls.push(encode(&changed));
    let mut changed = valid.clone();
    changed.insert(
        3,
        json!({"reason":"compiler-message","message":{"level":"error"}}),
    );
    controls.push(encode(&changed));
    controls.extend([
        b"{bad".to_vec(),
        b"null".to_vec(),
        vec![255],
        vec![0; REPORT_LIMIT as usize + 1],
        format!("{}{{}}", " ".repeat(LINE_LIMIT)).into_bytes(),
        "\n".repeat(RECORD_LIMIT + 1).into_bytes(),
    ]);
    let mut changed = valid.clone();
    changed[3]["features"] = json!(["development-probes"]);
    controls.push(encode(&changed));
    for bytes in &controls {
        if inspect(bytes, root).is_ok() {
            return Err("Production feature auditor failed a rejection control".into());
        }
    }
    Ok(controls.len())
}

fn lexical(path: &Path) -> Result<PathBuf, String> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|_| "Cannot resolve audit working directory")?
            .join(path)
    };
    let mut result = PathBuf::new();
    for part in absolute.components() {
        match part {
            Component::ParentDir => {
                result.pop();
            }
            Component::CurDir => {}
            other => result.push(other.as_os_str()),
        }
    }
    Ok(result)
}

pub(crate) fn verify_bootstrap(args: &[String], root: &Path) -> Result<bool, String> {
    if args.len() != 2 || args[0] != "--build-report" {
        return Err("Expected bootstrap --build-report PATH".into());
    }
    let bytes = regular(Path::new(&args[1]), REPORT_LIMIT)?;
    let mut report = inspect_bootstrap(&bytes)?;
    let suppliers = crate::vendor_audit::nix_source::resolve(root)?;
    let library = suppliers.library(&bytes, crate::vendor_audit::nix_source::Role::Bootstrap)?;
    report["terminal_supplier"] = library;
    println!("{}", serde_json::to_string_pretty(&report).unwrap());
    Ok(true)
}

fn inspect_bootstrap(bytes: &[u8]) -> Result<Value, String> {
    if bytes.len() as u64 > REPORT_LIMIT {
        return Err("Bootstrap report exceeds byte limit".into());
    }
    let text = std::str::from_utf8(bytes).map_err(|_| "Invalid bootstrap report UTF-8")?;
    if text.split('\n').count() > RECORD_LIMIT {
        return Err("Bootstrap report exceeds record limit".into());
    }
    let forbidden = [
        "autorouter_core",
        "autorouter_runtime",
        "hyper",
        "hyper_util",
        "openssl",
        "openssl_sys",
        "openssl_src",
        "tokio",
    ];
    let mut finished = false;
    let mut auditors = Vec::new();
    let mut packages = std::collections::BTreeSet::new();
    for line in text.split('\n') {
        if line.trim().is_empty() {
            continue;
        }
        if finished || line.len() > LINE_LIMIT {
            return Err("Invalid bootstrap report ordering or bounds".into());
        }
        let row: Value =
            serde_json::from_str(line).map_err(|_| "Invalid bootstrap Cargo record")?;
        if !row.is_object() {
            return Err("Invalid bootstrap Cargo record".into());
        }
        if row["reason"] == "build-finished" {
            if row["success"] != true {
                return Err("Bootstrap build did not succeed".into());
            }
            finished = true;
        }
        if row["reason"] == "compiler-message" && row["message"]["level"] == "error" {
            return Err("Bootstrap compiler emitted an error".into());
        }
        if row["reason"] != "compiler-artifact" {
            continue;
        }
        let package = row["package_id"]
            .as_str()
            .ok_or("Missing bootstrap package identity")?;
        let target = row["target"]["name"]
            .as_str()
            .ok_or("Missing bootstrap target identity")?;
        let normalized = package.replace('-', "_");
        let source = row["manifest_path"].as_str().unwrap_or("");
        if forbidden.contains(&target)
            || forbidden
                .iter()
                .any(|name| normalized.contains(&format!("#{name}@")))
            || [
                "vendor/hyper/",
                "vendor/hyper-util/",
                "vendor/openssl/",
                "crates/autorouter-runtime/",
                "crates/autorouter-core/",
            ]
            .iter()
            .any(|path| source.contains(path))
        {
            return Err("Bootstrap auditor compiled an inspected or runtime dependency".into());
        }
        packages.insert(package.to_owned());
        if target == "autorouter-audit" && includes(&row["target"]["kind"], "bin") {
            if row["features"]
                .as_array()
                .is_none_or(|features| !features.is_empty())
            {
                return Err("Bootstrap auditor must have no runtime/default features".into());
            }
            let executable = row["executable"]
                .as_str()
                .ok_or("Missing bootstrap executable")?;
            if !includes(&row["filenames"], executable) {
                return Err("Invalid bootstrap executable declaration".into());
            }
            auditors.push(executable.to_owned());
        }
    }
    if !finished || auditors.len() != 1 {
        return Err("Bootstrap report lacks a unique successful auditor build".into());
    }
    let binary = regular(Path::new(&auditors[0]), 256 * 1024 * 1024)?;
    Ok(
        json!({"schema_version":1,"kind":"native_auditor_bootstrap_build","passed":true,"report_sha256":digest(bytes),"binary_sha256":digest(&binary),"excluded_runtime_and_vendor_dependencies":true,"package_count":packages.len(),"package_identities_sha256":digest(&serde_json::to_vec(&packages).unwrap())}),
    )
}
pub fn run(args: &[String], root: &Path) -> Result<bool, String> {
    if args.len() == 2 && args[0] == "--bootstrap-report" {
        return verify_bootstrap(&["--build-report".into(), args[1].clone()], root);
    }
    if args == ["--help"] {
        println!(
            "Usage: cargo xtask verify-production-features --self-test | --build-report PATH\nNative ordinary release-artifact audit: exact pinned vendor packages, unique successful release build, each experimental feature and runtime development probes independently excluded, and actual binary checksum. No builds or execution of inspected binaries."
        );
        return Ok(true);
    }
    if args == ["--self-test"] {
        let rejected = self_test(root)?;
        println!(
            "Production feature auditor self-test: 1 accepted and {rejected} rejected controls (including each experimental feature and runtime development probes independently)."
        );
        return Ok(true);
    }
    if args.len() != 2 || args[0] != "--build-report" {
        return Err("Expected verify-production-features --self-test | --build-report PATH".into());
    }
    let bytes = regular(Path::new(&args[1]), REPORT_LIMIT)?;
    let mut inspected = inspect(&bytes, root)?;
    let suppliers = crate::vendor_audit::nix_source::resolve(root)?;
    inspected["terminal_supplier"] =
        suppliers.library(&bytes, crate::vendor_audit::nix_source::Role::Production)?;
    let executable = Path::new(inspected["binary"]["executable"].as_str().unwrap());
    if executable
        .canonicalize()
        .map_err(|_| "Cannot resolve release executable")?
        != lexical(executable)?
    {
        return Err("Release executable must not be a symlink".into());
    }
    let binary = regular(executable, 256 * 1024 * 1024)?;
    let report = json!({"passed":true,"report_sha256":digest(&bytes),"binary":executable,"binary_sha256":digest(&binary),"package_id":inspected["library"]["package_id"],"manifest_path":manifest(root,"vendor/hyper-util"),"features":inspected["library"]["features"],"lease_feature":false,"raw_pool_feature":false,"hyper_package_id":inspected["hyper_library"]["package_id"],"hyper_manifest_path":manifest(root,"vendor/hyper"),"hyper_features":inspected["hyper_library"]["features"],"body_handoff_feature":false,"runtime_package_id":inspected["runtime_library"]["package_id"],"runtime_manifest_path":manifest(root,"crates/autorouter-runtime"),"runtime_features":inspected["runtime_library"]["features"],"development_probes_feature":false,"terminal_supplier":inspected["terminal_supplier"]});
    println!("{}", serde_json::to_string_pretty(&report).unwrap());
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn independent_features_profiles_source_identity_and_malformed_cargo_records_fail() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .canonicalize()
            .unwrap();
        assert_eq!(self_test(&root).unwrap(), 46);
    }
    #[test]
    fn vendored_package_urls_handle_spaces_quotes_and_explicit_name_forms() {
        let root = Path::new("/synthetic repository ' with spaces");
        let mut valid = fixture(root);
        for (index, name, version) in [
            (0, "hyper-util", "0.1.21"),
            (1, "hyper", "1.12.0"),
            (3, "autorouter-runtime", env!("CARGO_PKG_VERSION")),
        ] {
            let id = valid[index]["package_id"].as_str().unwrap();
            valid[index]["package_id"] =
                json!(id.replace(&format!("#{version}"), &format!("#{name}@{version}")));
        }
        assert!(inspect(&encode(&valid), root).is_ok());
        valid.push(json!({"reason":"compiler-message"}));
        assert!(inspect(&encode(&valid), root).is_err());
    }

    #[test]
    fn bootstrap_rejects_inspected_dependencies_and_unexpected_feature_unification() {
        let scratch = crate::audit_support::Scratch::new("bootstrap-control").unwrap();
        let executable = scratch
            .file("autorouter-audit", b"synthetic nonexecuted binary")
            .unwrap();
        let auditor = json!({"reason":"compiler-artifact","package_id":"path+file:///synthetic/xtask#0.5.2","manifest_path":"/synthetic/xtask/Cargo.toml","target":{"name":"autorouter-audit","kind":["bin"]},"features":[],"executable":executable,"filenames":[executable]});
        let done = json!({"reason":"build-finished","success":true});
        assert_eq!(
            inspect_bootstrap(&encode(&[auditor.clone(), done.clone()])).unwrap()["passed"],
            true
        );
        let mut changed = auditor.clone();
        changed["features"] = json!(["runtime-tools"]);
        assert!(inspect_bootstrap(&encode(&[changed, done.clone()])).is_err());
        for name in [
            "hyper",
            "hyper_util",
            "openssl",
            "tokio",
            "autorouter_runtime",
        ] {
            let dependency = json!({"reason":"compiler-artifact","package_id":format!("registry+synthetic#{name}@1.0.0"),"target":{"name":"build-script-build"},"manifest_path":"/synthetic/dependency/Cargo.toml"});
            assert!(
                inspect_bootstrap(&encode(&[dependency, auditor.clone(), done.clone()])).is_err()
            );
        }
        assert!(inspect_bootstrap(&encode(&[auditor])).is_err());
    }

    #[test]
    fn bootstrap_reports_do_not_echo_unknown_record_metadata() {
        let scratch = crate::audit_support::Scratch::new("bootstrap-privacy").unwrap();
        let executable = scratch
            .file("autorouter-audit", b"synthetic nonexecuted binary")
            .unwrap();
        let canary = "SYNTHETIC_PRIVATE_COMPILER_RECORD";
        let rows = [
            json!({"reason":"compiler-artifact","package_id":canary,"manifest_path":canary,"target":{"name":"synthetic_dependency"},"other":canary}),
            json!({"reason":"compiler-artifact","package_id":"synthetic-auditor","target":{"name":"autorouter-audit","kind":["bin"]},"features":[],"executable":executable,"filenames":[executable]}),
            json!({"reason":"build-finished","success":true}),
        ];
        let report = inspect_bootstrap(&encode(&rows)).unwrap();
        assert!(!serde_json::to_string(&report).unwrap().contains(canary));
        assert_eq!(report["package_count"], 2);
    }
}
