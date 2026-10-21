//! Explicit native-only supplier controls; never part of shipping runtime dependencies.
use crate::audit_support::{Scratch, digest, regular};
use crate::process::capture_result;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Component, Path, PathBuf};
use std::process::Command;
use std::time::Duration;

const MANIFEST: &str = "rust/supplier-tests/openssl-native/Cargo.toml";
const LOCK: &str = "rust/supplier-tests/openssl-native/Cargo.lock";
const MANIFEST_SHA: &str = "762fb2b96019266f88da8e4ba577c18d14a3a31a4470c0d4181c005b5ddb30c2";
const LOCK_SHA: &str = "90694215b6d2d8d5eb06bae5c7fa7f6334e8989de663d611017cc4c611bac7e3";
const ROOT_LOCK_SHA: &str = "014088b4b3e5123311b1115193e7c6c7b0fa3cbfe5c7a036106956dad066d36a";
const ORIGINAL_LOCK_SHA: &str = "710c8e66450a426af6e2fdf69871c2e3f8860924c75da8ed868439c73bf2137b";
const PREFIX: &str = "ssl::node_defaults::tests::";
const CHILD: &str = "ssl::node_defaults::tests::first_use_gate_child";
const MARKER: &str = "Node-default first-use gate verified";
// Closed separately: decoder and actual BIO callback units are not TLS probes.
const DECODER_PREFIX: &str = "error::system_reason_tests::";
const DECODER_NAMES: &[&str] = &[
    "mixed_and_nonsystem_roundtrips_preserve_order_and_flags",
    "system_error_roundtrip_preserves_raw_code_and_reason",
    "system_reasons_keep_all_public_mask_bits",
];
const BIO_PREFIX: &str = "ssl::bio::write_errno_tests::";
const BIO_NAMES: &[&str] = &[
    "custom_error_keeps_original_payload_without_an_os_code",
    "os_error_is_preserved_across_actual_bio_write",
    "raw_retry_error_keeps_bio_retry_and_owned_errno",
];
const WANT_PREFIX: &str = "ssl::want_tests::";
const WANT_NAMES: &[&str] = &[
    "new_ssl_want_preserves_the_actual_error_queue",
    "nonblocking_connect_want_read_is_observed_after_real_ssl_call",
    "nonblocking_connect_want_write_is_observed_after_real_ssl_call",
];
const NAMES: &[&str] = &[
    "callback_panic_is_contained_and_constructor_unwind_drops_scope",
    "first_use_gate_child",
    "invalid_groups_reject_real_context_and_allow_clean_retry",
    "nested_guard_rejects_before_a_second_constructor_and_preserves_outer",
    "real_null_method_and_owned_post_callback_failure_clear_scope",
    "synchronized_threads_keep_invalid_valid_and_unarmed_scopes_independent",
    "two_live_contexts_bind_distinct_parents_and_leave_unarmed_defaults",
    "wrong_missing_and_duplicate_callback_records_reject_owned_contexts",
];
// Versions/edges are the reviewed runtime closure plus developer-only hex.
const PACKAGES: &str = r#"{"bitflags":{"dependencies":[],"source":"registry+https://github.com/rust-lang/crates.io-index","version":"2.13.2"},"cc":{"dependencies":["find-msvc-tools","shlex"],"source":"registry+https://github.com/rust-lang/crates.io-index","version":"1.6.0"},"cfg-if":{"dependencies":[],"source":"registry+https://github.com/rust-lang/crates.io-index","version":"1.0.5"},"find-msvc-tools":{"dependencies":[],"source":"registry+https://github.com/rust-lang/crates.io-index","version":"0.1.14"},"foreign-types":{"dependencies":["foreign-types-shared"],"source":"registry+https://github.com/rust-lang/crates.io-index","version":"0.3.2"},"foreign-types-shared":{"dependencies":[],"source":"registry+https://github.com/rust-lang/crates.io-index","version":"0.1.1"},"hex":{"dependencies":[],"source":"registry+https://github.com/rust-lang/crates.io-index","version":"0.4.3"},"libc":{"dependencies":[],"source":"registry+https://github.com/rust-lang/crates.io-index","version":"0.2.190"},"openssl":{"dependencies":["bitflags","cfg-if","foreign-types","hex","libc","openssl-macros","openssl-sys"],"source":null,"version":"0.10.81"},"openssl-macros":{"dependencies":["proc-macro2","quote","syn"],"source":"registry+https://github.com/rust-lang/crates.io-index","version":"0.1.1"},"openssl-src":{"dependencies":["cc"],"source":"registry+https://github.com/rust-lang/crates.io-index","version":"300.6.1+3.6.3"},"openssl-sys":{"dependencies":["cc","libc","openssl-src","pkg-config","vcpkg"],"source":"registry+https://github.com/rust-lang/crates.io-index","version":"0.9.117"},"pkg-config":{"dependencies":[],"source":"registry+https://github.com/rust-lang/crates.io-index","version":"0.3.34"},"proc-macro2":{"dependencies":["unicode-ident"],"source":"registry+https://github.com/rust-lang/crates.io-index","version":"1.0.107"},"quote":{"dependencies":["proc-macro2"],"source":"registry+https://github.com/rust-lang/crates.io-index","version":"1.0.47"},"shlex":{"dependencies":[],"source":"registry+https://github.com/rust-lang/crates.io-index","version":"2.0.1"},"syn":{"dependencies":["proc-macro2","quote","unicode-ident"],"source":"registry+https://github.com/rust-lang/crates.io-index","version":"2.0.119"},"unicode-ident":{"dependencies":[],"source":"registry+https://github.com/rust-lang/crates.io-index","version":"1.0.26"},"vcpkg":{"dependencies":[],"source":"registry+https://github.com/rust-lang/crates.io-index","version":"0.2.15"}}"#;

fn text(value: &Value) -> Result<&str, String> {
    value
        .as_str()
        .ok_or_else(|| "Invalid supplier metadata string".into())
}
fn require(condition: bool, message: &str) -> Result<(), String> {
    if condition {
        Ok(())
    } else {
        Err(message.into())
    }
}
fn same_path(value: &Value, expected: &Path) -> bool {
    value.as_str().and_then(|path| fs::canonicalize(path).ok()) == fs::canonicalize(expected).ok()
        && fs::canonicalize(expected).is_ok()
}
fn save(out: &Path, name: &str, bytes: &[u8]) -> Result<(), String> {
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(out.join(name))
        .and_then(|mut file| file.write_all(bytes))
        .map_err(|_| "Cannot retain supplier evidence".into())
}
fn command(program: &Path, root: &Path) -> Command {
    let mut command = Command::new(program);
    command.current_dir(root.join("rust")).env_clear();
    for name in [
        "PATH",
        "HOME",
        "CARGO_HOME",
        "RUSTUP_HOME",
        "RUSTUP_TOOLCHAIN",
        "TMPDIR",
        "TMP",
        "TEMP",
        "MACOSX_DEPLOYMENT_TARGET",
        "SDKROOT",
    ] {
        if let Some(value) = std::env::var_os(name) {
            command.env(name, value);
        }
    }
    command
        .env("CARGO_NET_OFFLINE", "true")
        .env("OPENSSL_NO_VENDOR", "0")
        .env("OPENSSL_CONF", "")
        .env("CARGO_INCREMENTAL", "0")
        .env("LANG", "C")
        .env("LC_ALL", "C")
        .env("TZ", "UTC");
    command
}
// Caller selects an existing build cache. Resolve no symlink component and
// create/delete nothing. Source roots outside the two cache namespaces do not qualify.
fn admit_target(root: &Path, relative: &Path) -> Result<PathBuf, String> {
    let parts: Vec<_> = relative.components().collect();
    require(
        !parts.is_empty()
            && parts
                .iter()
                .all(|part| matches!(part, Component::Normal(_))),
        "Supplier target must be a repository-relative directory without traversal",
    )?;
    require(
        relative.starts_with("rust/target")
            || (relative.starts_with("artifacts") && parts.len() > 1),
        "Supplier target must be under rust/target or artifacts",
    )?;
    let canonical_root = root
        .canonicalize()
        .map_err(|_| "Cannot resolve repository root")?;
    let mut target = canonical_root.clone();
    for part in parts {
        target.push(part);
        let info =
            fs::symlink_metadata(&target).map_err(|_| "Supplier target must already exist")?;
        require(
            info.is_dir() && !info.file_type().is_symlink(),
            "Supplier target components must be real existing directories",
        )?;
    }
    let canonical = target
        .canonicalize()
        .map_err(|_| "Cannot resolve selected supplier target")?;
    require(
        canonical.starts_with(&canonical_root)
            && canonical != canonical_root
            && canonical == target,
        "Supplier target escaped repository or changed identity",
    )?;
    Ok(canonical)
}
fn phase(
    out: &Path,
    name: &str,
    command: &mut Command,
    seconds: u64,
    report: &mut Value,
) -> Result<Vec<u8>, String> {
    // Shared capture currently bounds stdout128MiB/stderr64KiB and owns/reaps its group.
    // The lower8MiB record admission below is not claimed as a lower live capture cap.
    let captured = capture_result(command, &[], Duration::from_secs(seconds))?;
    save(out, &format!("{name}.stdout"), &captured.stdout)?;
    save(out, &format!("{name}.stderr"), &captured.stderr)?;
    report["phases"]
        .as_array_mut()
        .ok_or("Invalid supplier phase report")?
        .push(json!({
            "name":name, "success":captured.status.success(), "exit_code":captured.status.code(),
            "stdout_bytes":captured.stdout.len(), "stdout_sha256":digest(&captured.stdout),
            "stderr_bytes":captured.stderr.len(), "stderr_sha256":digest(&captured.stderr)
        }));
    require(
        captured.status.success(),
        "Supplier check child failed; inspect retained private phase evidence",
    )?;
    require(
        captured.stdout.len() <= 8 * 1024 * 1024,
        "Supplier record exceeds8MiB admission limit",
    )?;
    Ok(captured.stdout)
}
fn pins(root: &Path) -> Result<BTreeMap<String, String>, String> {
    let provenance: Value = serde_json::from_slice(&regular(
        &root.join("rust/vendor/openssl-provenance.json"),
        1024 * 1024,
    )?)
    .map_err(|_| "Invalid supplier provenance")?;
    let files = provenance["patched_files"]
        .as_object()
        .ok_or("Missing supplier file map")?;
    require(files.len() == 114, "Supplier file count differs")?;
    let mut result = BTreeMap::new();
    for (name, expected) in files {
        require(
            Path::new(name)
                .components()
                .all(|part| matches!(part, Component::Normal(_))),
            "Invalid supplier path",
        )?;
        let path = format!("rust/vendor/openssl/{name}");
        let actual = digest(&regular(&root.join(&path), 16 * 1024 * 1024)?);
        require(
            actual == text(expected)?,
            "Supplier bytes differ from reviewed provenance",
        )?;
        result.insert(path, actual);
    }
    for (path, expected) in [
        (MANIFEST, MANIFEST_SHA),
        (LOCK, LOCK_SHA),
        ("rust/Cargo.lock", ROOT_LOCK_SHA),
        ("rust/vendor/openssl/Cargo.lock", ORIGINAL_LOCK_SHA),
    ] {
        let actual = digest(&regular(&root.join(path), 1024 * 1024)?);
        require(
            actual == expected,
            "Supplier harness or authoritative lock drift requires explicit review",
        )?;
        result.insert(path.into(), actual);
    }
    result.insert(
        "rust/vendor/openssl-provenance.json".into(),
        digest(&regular(
            &root.join("rust/vendor/openssl-provenance.json"),
            1024 * 1024,
        )?),
    );
    let patch_hash = digest(&regular(
        &root.join("rust/vendor/openssl-node-trust.patch"),
        2 * 1024 * 1024,
    )?);
    require(
        patch_hash == text(&provenance["patch_sha256"])?,
        "Supplier patch differs from current admitted provenance",
    )?;
    result.insert("rust/vendor/openssl-node-trust.patch".into(), patch_hash);
    for path in [
        "rust/xtask/src/vendor_audit.rs",
        "rust/xtask/src/supplier_units.rs",
    ] {
        result.insert(
            path.into(),
            digest(&regular(&root.join(path), 1024 * 1024)?),
        );
    }
    Ok(result)
}

fn metadata(root: &Path, value: &Value) -> Result<BTreeMap<String, String>, String> {
    let expected: BTreeMap<String, Value> =
        serde_json::from_str(PACKAGES).map_err(|_| "Invalid compiled supplier descriptor")?;
    let packages = value["packages"]
        .as_array()
        .ok_or("Missing supplier packages")?;
    let nodes = value["resolve"]["nodes"]
        .as_array()
        .ok_or("Missing supplier resolve graph")?;
    require(
        packages.len() == 19 && nodes.len() == 19,
        "Supplier graph must contain only runtime18 plus developer hex",
    )?;
    let mut ids = BTreeMap::new();
    for package in packages {
        let name = text(&package["name"])?;
        let wanted = expected.get(name).ok_or("Unexpected supplier dependency")?;
        require(
            package["version"] == wanted["version"] && package["source"] == wanted["source"],
            "Supplier dependency identity differs",
        )?;
        require(
            ids.insert(name.to_owned(), text(&package["id"])?.to_owned())
                .is_none(),
            "Duplicate supplier package",
        )?;
        if name == "openssl" {
            require(
                same_path(&package["manifest_path"], &root.join(MANIFEST)),
                "Supplier must use isolated native-only manifest",
            )?;
            let targets = package["targets"]
                .as_array()
                .ok_or("Missing supplier targets")?;
            require(
                targets.iter().any(|target| {
                    target["kind"] == json!(["lib"])
                        && same_path(
                            &target["src_path"],
                            &root.join("rust/vendor/openssl/src/lib.rs"),
                        )
                }),
                "Supplier lib must use audited original source",
            )?;
        }
    }
    require(
        value["workspace_members"] == json!([ids["openssl"]]),
        "Supplier workspace must be isolated",
    )?;
    for (name, id) in &ids {
        let node = nodes
            .iter()
            .find(|node| node["id"].as_str() == Some(id.as_str()))
            .ok_or("Missing supplier resolved node")?;
        let actual: BTreeSet<String> = node["dependencies"]
            .as_array()
            .ok_or("Missing dependency edges")?
            .iter()
            .map(|value| text(value).map(str::to_owned))
            .collect::<Result<_, _>>()?;
        let wanted: BTreeSet<String> = expected[name]["dependencies"]
            .as_array()
            .ok_or("Invalid compiled supplier edges")?
            .iter()
            .map(|value| {
                ids.get(text(value)?)
                    .cloned()
                    .ok_or_else(|| "Missing supplier dependency identity".into())
            })
            .collect::<Result<_, String>>()?;
        require(actual == wanted, "Supplier dependency edges differ")?;
        let features = match name.as_str() {
            "openssl" => Some(json!(["default", "vendored"])),
            "openssl-sys" => Some(json!(["openssl-src", "vendored"])),
            "openssl-src" => Some(json!(["default", "legacy"])),
            _ => None,
        };
        if let Some(expected) = features {
            require(
                node["features"] == expected,
                "Supplier backend features differ",
            )?;
        }
    }
    Ok(ids)
}
fn version_header(bytes: &[u8]) -> Result<(), String> {
    let source = std::str::from_utf8(bytes).map_err(|_| "Invalid OpenSSL version header")?;
    for (name, value) in [("MAJOR", "3"), ("MINOR", "6"), ("PATCH", "3")] {
        let expression = regex::Regex::new(&format!(
            r"(?m)^\s*#\s*define\s+OPENSSL_VERSION_{name}\s+{value}\s*$"
        ))
        .map_err(|_| "Invalid version guard")?;
        require(
            expression.is_match(source),
            "Linked supplier header must be native OpenSSL3.6.3",
        )?;
    }
    Ok(())
}
fn select_artifact<'a>(rows: &'a [Value], id: &str) -> Result<&'a Value, String> {
    let selected: Vec<_> = rows
        .iter()
        .filter(|row| {
            row["reason"] == "compiler-artifact"
                && row["package_id"] == id
                && row["target"]["name"] == "openssl"
                && row["profile"]["test"] == true
                && row["executable"].is_string()
        })
        .collect();
    require(
        selected.len() == 1,
        "Expected one actual supplier libtest artifact",
    )?;
    let artifact = selected[0];
    require(
        artifact["target"]["kind"] == json!(["lib"])
            && artifact["features"] == json!(["default", "vendored"])
            && artifact["profile"]["opt_level"] == "0"
            && artifact["profile"]["debuginfo"] == 0,
        "Supplier libtest profile/features differ",
    )?;
    Ok(artifact)
}
fn binary(
    root: &Path,
    target: &Path,
    ids: &BTreeMap<String, String>,
    bytes: &[u8],
    report: &mut Value,
) -> Result<(PathBuf, String), String> {
    let rows = std::str::from_utf8(bytes)
        .map_err(|_| "Invalid Cargo record encoding")?
        .lines()
        .map(|line| {
            serde_json::from_str::<Value>(line).map_err(|_| "Invalid Cargo JSON record".to_owned())
        })
        .collect::<Result<Vec<_>, _>>()?;
    require(
        rows.last() == Some(&json!({"reason":"build-finished","success":true})),
        "Supplier build did not finish cleanly",
    )?;
    let artifact = select_artifact(&rows, &ids["openssl"])?;
    require(
        same_path(&artifact["manifest_path"], &root.join(MANIFEST))
            && same_path(
                &artifact["target"]["src_path"],
                &root.join("rust/vendor/openssl/src/lib.rs"),
            ),
        "Supplier artifact source differs",
    )?;
    let relative = target
        .strip_prefix(root)
        .map_err(|_| "Selected supplier target escaped repository")?;
    require(
        admit_target(root, relative)? == target,
        "Selected supplier target changed before artifact admission",
    )?;
    let path = PathBuf::from(text(&artifact["executable"])?);
    require(
        path.canonicalize()
            .is_ok_and(|value| value.starts_with(target)),
        "Supplier executable escaped selected target",
    )?;
    for (name, expected) in [
        ("openssl-sys", json!(["openssl-src", "vendored"])),
        ("openssl-src", json!(["default", "legacy"])),
    ] {
        let actual: Vec<_> = rows
            .iter()
            .filter(|row| {
                row["reason"] == "compiler-artifact"
                    && row["package_id"].as_str() == Some(ids[name].as_str())
                    && row["target"]["kind"] == json!(["lib"])
            })
            .collect();
        require(
            actual.len() == 1
                && actual[0]["features"] == expected
                && actual[0]["profile"]["test"] == false,
            "Supplier native build artifact features differ from admitted metadata",
        )?;
    }
    let scripts: Vec<_> = rows
        .iter()
        .filter(|row| {
            row["reason"] == "build-script-executed" && row["package_id"] == ids["openssl-sys"]
        })
        .collect();
    require(scripts.len() == 1, "Expected one OpenSSL-sys build record")?;
    let script = scripts[0];
    require(
        script["linked_libs"] == json!(["static=ssl", "static=crypto"]),
        "Native supplier must link vendored static SSL/crypto",
    )?;
    let paths = script["linked_paths"]
        .as_array()
        .ok_or("Missing native link paths")?;
    require(paths.len() == 1, "Ambiguous native link directory")?;
    let library = PathBuf::from(
        text(&paths[0])?
            .strip_prefix("native=")
            .ok_or("Invalid native link directory")?,
    );
    let directory = library
        .canonicalize()
        .map_err(|_| "Cannot resolve native libraries")?;
    let out = PathBuf::from(text(&script["out_dir"])?)
        .canonicalize()
        .map_err(|_| "Cannot resolve OpenSSL-sys output")?;
    require(
        directory.starts_with(&out) && out.starts_with(target),
        "Native link path escaped Cargo output",
    )?;
    let header = directory
        .parent()
        .ok_or("Missing native install root")?
        .join("include/openssl/opensslv.h");
    let header_bytes = regular(&header, 64 * 1024)?;
    version_header(&header_bytes)?;
    let mut libraries = BTreeMap::new();
    for name in ["libssl.a", "libcrypto.a"] {
        libraries.insert(
            name,
            digest(&regular(&directory.join(name), 64 * 1024 * 1024)?),
        );
    }
    let hash = digest(&regular(&path, 64 * 1024 * 1024)?);
    report["artifact"] = json!({"sha256":hash,"profile":artifact["profile"],"features":artifact["features"],
        "native_version_header_sha256":digest(&header_bytes),"static_library_sha256":libraries,
        "backend":"OpenSSL3.6.3 metadata/header/static link records before any test child; constructor runtime version gate is exercised by the tests"});
    Ok((path, hash))
}
fn outcomes(bytes: &[u8], names: &[String]) -> Result<(), String> {
    let output = std::str::from_utf8(bytes).map_err(|_| "Invalid supplier test output")?;
    let actual: Vec<String> = output
        .lines()
        .filter_map(|line| {
            line.strip_prefix("test ")
                .and_then(|line| line.strip_suffix(" ... ok"))
                .map(str::to_owned)
        })
        .collect();
    require(actual == names, "Supplier exact test names/statuses differ")?;
    let total = format!(
        "test result: ok. {} passed; 0 failed; 0 ignored; 0 measured;",
        names.len()
    );
    require(
        output
            .lines()
            .filter(|line| line.starts_with("test result:"))
            .count()
            == 1
            && output.contains(&total),
        "Supplier test totals differ",
    )
}
// List before execute: extra/missing/ignored or renamed controls fail closed.
// Keep this supplier decoder/BIO evidence separate from real runtime TLS controls.
fn closed_supplier_group(
    root: &Path,
    out: &Path,
    executable: &Path,
    hash: &str,
    descriptor: (&str, &str, &[&str]),
    report: &mut Value,
) -> Result<(), String> {
    let (group, prefix, suffixes) = descriptor;
    let names: Vec<String> = suffixes
        .iter()
        .map(|name| format!("{prefix}{name}"))
        .collect();
    require(!names.is_empty(), "Supplier unit group must not be empty")?;
    require(
        names.windows(2).all(|pair| pair[0] < pair[1]),
        "Supplier unit descriptors must be unique and ordered",
    )?;
    require(
        digest(&regular(executable, 64 * 1024 * 1024)?) == hash,
        "Supplier executable changed before unit discovery",
    )?;
    let mut listing = command(executable, root);
    listing.args([prefix, "--list"]);
    let output = phase(out, &format!("{group}-list"), &mut listing, 60, report)?;
    let listed: Vec<String> = std::str::from_utf8(&output)
        .map_err(|_| "Invalid supplier unit list")?
        .lines()
        .filter_map(|line| line.strip_suffix(": test").map(str::to_owned))
        .collect();
    require(
        listed == names,
        "Supplier unit selector names differ from closed reviewed group",
    )?;
    require(
        digest(&regular(executable, 64 * 1024 * 1024)?) == hash,
        "Supplier executable changed before unit controls",
    )?;
    let mut controls = command(executable, root);
    controls.args([prefix, "--nocapture", "--test-threads=1"]);
    outcomes(
        &phase(out, &format!("{group}-tests"), &mut controls, 60, report)?,
        &names,
    )?;
    report["supplier_groups"].as_array_mut().ok_or("Invalid supplier group report")?.push(json!({
        "group":group, "prefix":prefix, "closed_names":names, "passed":suffixes.len(), "failed":0, "ignored":0,
        "scope":"Supplier decoder/BIO/public-WANT controls only; runtime real TLS protocol branches remain separate"
    }));
    Ok(())
}
fn fresh_marker(output: &[u8], stderr: &[u8]) -> Result<(), String> {
    let count = String::from_utf8_lossy(output).matches(MARKER).count()
        + String::from_utf8_lossy(stderr).matches(MARKER).count();
    require(
        count == 1,
        "Fresh supplier initialization guard marker missing or duplicated",
    )
}
fn source_admission(root: &Path) -> Result<(), String> {
    let mut audit = command(
        &std::env::current_exe().map_err(|_| "Cannot resolve independent auditor")?,
        root,
    );
    audit.args(["vendor", "--only", "openssl"]);
    let result = capture_result(&mut audit, &[], Duration::from_secs(120))?;
    require(result.status.success(), "Independent supplier audit failed")?;
    let value: Value =
        serde_json::from_slice(&result.stdout).map_err(|_| "Invalid supplier audit JSON")?;
    require(
        value["passed"] == true,
        "Independent supplier source audit rejected",
    )?;
    pins(root)?;
    Ok(())
}
fn exercise(root: &Path, out: &Path, target: &Path, report: &mut Value) -> Result<(), String> {
    let mut auditor = command(
        &std::env::current_exe().map_err(|_| "Cannot resolve independent auditor")?,
        root,
    );
    auditor.args(["vendor", "--only", "openssl"]);
    let vendor = phase(out, "vendor", &mut auditor, 120, report)?;
    require(
        serde_json::from_slice::<Value>(&vendor).map_err(|_| "Invalid supplier audit result")?["passed"]
            == true,
        "Independent supplier audit failed",
    )?;
    let before = pins(root)?;
    let result = (|| {
        let mut inspect = command(Path::new("cargo"), root);
        inspect.args([
            "metadata",
            "--manifest-path",
            "supplier-tests/openssl-native/Cargo.toml",
            "--format-version",
            "1",
            "--features",
            "vendored",
            "--locked",
            "--offline",
        ]);
        let value: Value =
            serde_json::from_slice(&phase(out, "metadata", &mut inspect, 60, report)?)
                .map_err(|_| "Invalid supplier metadata JSON")?;
        let ids = metadata(root, &value)?;
        let mut build = command(Path::new("cargo"), root);
        build
            .args([
                "test",
                "--manifest-path",
                "supplier-tests/openssl-native/Cargo.toml",
                "--lib",
                "--features",
                "vendored",
                "--locked",
                "--offline",
                "-j2",
                "--no-run",
                "--message-format=json",
                "--target-dir",
            ])
            .arg(target);
        let records = phase(out, "build", &mut build, 900, report)?;
        let (executable, hash) = binary(root, target, &ids, &records, report)?;
        let mut listing = command(&executable, root);
        listing.args([PREFIX, "--list"]);
        let listed = phase(out, "list", &mut listing, 60, report)?;
        let names: Vec<String> = NAMES.iter().map(|name| format!("{PREFIX}{name}")).collect();
        let listed: Vec<String> = std::str::from_utf8(&listed)
            .map_err(|_| "Invalid supplier list")?
            .lines()
            .filter_map(|line| line.strip_suffix(": test").map(str::to_owned))
            .collect();
        require(
            listed == names,
            "Supplier selector must expose exactly the closed8 names",
        )?;
        require(
            digest(&regular(&executable, 64 * 1024 * 1024)?) == hash,
            "Supplier executable changed before ordinary controls",
        )?;
        let mut ordinary = command(&executable, root);
        ordinary.args([PREFIX, "--nocapture", "--test-threads=1"]);
        outcomes(&phase(out, "ordinary", &mut ordinary, 60, report)?, &names)?;
        closed_supplier_group(
            root,
            out,
            &executable,
            &hash,
            ("reason-decoder", DECODER_PREFIX, DECODER_NAMES),
            report,
        )?;
        closed_supplier_group(
            root,
            out,
            &executable,
            &hash,
            ("write-errno", BIO_PREFIX, BIO_NAMES),
            report,
        )?;
        closed_supplier_group(
            root,
            out,
            &executable,
            &hash,
            ("public-want", WANT_PREFIX, WANT_NAMES),
            report,
        )?;
        let scratch = Scratch::new("supplier-first-use")?;
        let failure_file = scratch.file("invalid.cnf", b"[owned_invalid_section\n")?;
        report["fresh_controls"] = json!([]);
        for mode in ["success", "failure"] {
            require(
                digest(&regular(&executable, 64 * 1024 * 1024)?) == hash,
                "Supplier executable changed before fresh child",
            )?;
            let mut child = command(&executable, root);
            child
                .args(["--exact", CHILD, "--nocapture", "--test-threads=1"])
                .env("AUTOROUTER_SYNTHETIC_NODE_DEFAULTS_INIT", mode);
            if mode == "failure" {
                child.env("AUTOROUTER_SYNTHETIC_NODE_DEFAULTS_FILE", &failure_file);
            }
            let label = format!("fresh-{mode}");
            let output = phase(out, &label, &mut child, 60, report)?;
            outcomes(&output, &[CHILD.to_owned()])?;
            let stderr = regular(&out.join(format!("{label}.stderr")), 64 * 1024)?;
            fresh_marker(&output, &stderr)?;
            report["fresh_controls"].as_array_mut().ok_or("Invalid fresh control report")?.push(
                json!({"mode":mode,"passed":1,"ignored":0,"exact_name":CHILD,"marker_verified":true}));
        }
        report["closed_names"] = json!(names);
        report["ordinary_passed"] = json!(8);
        report["reason_decoder_passed"] = json!(DECODER_NAMES.len());
        report["write_errno_passed"] = json!(BIO_NAMES.len());
        report["public_want_passed"] = json!(WANT_NAMES.len());
        report["fresh_modes"] = json!(["success", "failure"]);
        Ok(())
    })();
    let preserved = pins(root)?;
    require(
        preserved == before,
        "Supplier originals or authoritative locks changed during checks",
    )?;
    report["preserved_files"] = json!(before.len());
    report["input_sha256"] = json!(before);
    result
}

pub fn run(args: &[String], root: &Path) -> Result<bool, String> {
    if matches!(args, [flag] if flag == "--help") {
        println!(
            "Usage: autorouter-audit supplier-units --check-harness\n       autorouter-audit supplier-units --out REPOSITORY_RELATIVE_NEW_DIRECTORY [--target-dir REPOSITORY_RELATIVE_EXISTING_CACHE]\nDefault target rust/target; existing non-symlink repository directories under rust/target or artifacts only. No target directory creation/deletion; CARGO_INCREMENTAL=0.\nSource-admit the exact native-only manifest/lock before explicit locked setup fetch. Then offline context8+fresh2, decoder3, BIO3 and public-WANT3 supplier controls. No Node/runtime sidecar, provider calls or production qualification."
        );
        return Ok(true);
    }
    if matches!(args, [flag] if flag == "--check-harness") {
        source_admission(root)?;
        println!(
            "{}",
            json!({"passed":true,"kind":"NATIVE_SUPPLIER_HARNESS_SOURCE_ADMISSION","graph":"runtime18 plus developer hex; inspected code not executed"})
        );
        return Ok(true);
    }
    let (directory, target_relative) = match args {
        [flag, directory] if flag == "--out" => (directory.as_str(), "rust/target"),
        [flag, directory, target_flag, target]
            if flag == "--out" && target_flag == "--target-dir" =>
        {
            (directory.as_str(), target.as_str())
        }
        _ => return Err(
            "Expected supplier-units --out NEW_DIRECTORY [--target-dir EXISTING_REPOSITORY_CACHE]"
                .into(),
        ),
    };
    let target = admit_target(root, Path::new(target_relative))?;
    let relative = Path::new(directory);
    require(
        relative
            .components()
            .all(|part| matches!(part, Component::Normal(_)))
            && relative.starts_with("artifacts"),
        "Supplier evidence must use a new repository-relative artifacts directory",
    )?;
    let out = root.join(relative);
    let parent = out.parent().ok_or("Missing supplier evidence parent")?;
    require(
        fs::canonicalize(parent).is_ok_and(|path| path.starts_with(root)),
        "Supplier evidence parent must exist within repository",
    )?;
    fs::DirBuilder::new()
        .mode(0o700)
        .create(&out)
        .map_err(|_| "Supplier evidence directory must be new")?;
    let mut report = json!({"schema_version":1,"kind":"NATIVE_ONLY_OPENSSL_SUPPLIER_CONTROLS","passed":false,"phases":[],"supplier_groups":[],
        "ordinary_scope":"7 active context controls plus inactive ordinary first-use entrypoint; fresh guards run separately. Decoder3, BIO3 and public-WANT3 are separate closed unit groups, not real runtime TLS probes.",
        "limits":"Shared capture128MiB stdout/64KiB stderr;8MiB admitted records;120s audit/60s metadata/900s build/60s each discovery or test phase;13 phases total, sum1680s plus overhead. Normal/error/timeout/Drop owned-group cleanup; no universal abrupt outer-cancellation/crash descendant guarantee. No production/backend/platform/performance claim."});
    report["target_dir"] = json!(target_relative);
    report["incremental"] = json!(false);
    let result = exercise(root, &out, &target, &mut report);
    report["passed"] = json!(result.is_ok());
    if let Err(error) = result {
        report["failure"] = json!(error);
    }
    save(
        &out,
        "report.json",
        &serde_json::to_vec_pretty(&report).map_err(|_| "Cannot encode supplier report")?,
    )?;
    println!(
        "{}",
        serde_json::to_string_pretty(&report).map_err(|_| "Cannot encode supplier report")?
    );
    Ok(report["passed"] == true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn supplier_target_rejects_missing_external_traversal_and_symlink_directories() {
        let scratch = Scratch::new("supplier-target-admission").unwrap();
        let root = &scratch.0;
        fs::create_dir_all(root.join("rust/target")).unwrap();
        fs::create_dir_all(root.join("artifacts/owned/correctness-target")).unwrap();
        fs::create_dir_all(root.join("rust/crates/source")).unwrap();
        assert_eq!(
            admit_target(root, Path::new("rust/target")).unwrap(),
            root.join("rust/target")
        );
        assert_eq!(
            admit_target(root, Path::new("artifacts/owned/correctness-target")).unwrap(),
            root.join("artifacts/owned/correctness-target")
        );
        for rejected in [
            "",
            "/tmp",
            "../target",
            "rust/target/../../crates/source",
            "rust/targeted",
            "rust/crates/source",
            "artifacts",
            "artifacts/missing",
        ] {
            assert!(
                admit_target(root, Path::new(rejected)).is_err(),
                "{rejected}"
            );
        }
        fs::write(root.join("artifacts/not-a-directory"), b"synthetic").unwrap();
        assert!(admit_target(root, Path::new("artifacts/not-a-directory")).is_err());
        std::os::unix::fs::symlink(
            root.join("rust/target"),
            root.join("artifacts/linked-target"),
        )
        .unwrap();
        assert!(admit_target(root, Path::new("artifacts/linked-target")).is_err());
        std::os::unix::fs::symlink(
            root.join("artifacts/owned"),
            root.join("artifacts/linked-parent"),
        )
        .unwrap();
        assert!(
            admit_target(
                root,
                Path::new("artifacts/linked-parent/correctness-target")
            )
            .is_err()
        );
        assert!(!root.join("artifacts/missing").exists());
    }
    #[test]
    fn supplier_version_guard_rejects_old_and_mixed_headers() {
        let good = b"# define OPENSSL_VERSION_MAJOR 3\n# define OPENSSL_VERSION_MINOR 6\n# define OPENSSL_VERSION_PATCH 3\n";
        assert!(version_header(good).is_ok());
        assert!(
            version_header(
                String::from_utf8_lossy(good)
                    .replace("PATCH 3", "PATCH 2")
                    .as_bytes()
            )
            .is_err()
        );
        assert!(
            version_header(b"# define OPENSSL_VERSION_MAJOR 3\n# define OPENSSL_VERSION_MINOR 6\n")
                .is_err()
        );
    }
    #[test]
    fn ignored_or_missing_fresh_test_cannot_be_a_success() {
        let names = [CHILD.to_owned()];
        let good = format!(
            "test {CHILD} ... ok\ntest result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 473 filtered out;\n"
        );
        assert!(outcomes(good.as_bytes(), &names).is_ok());
        assert!(outcomes(good.replace(" ... ok", " ... ignored").as_bytes(), &names).is_err());
        assert!(outcomes(good.replace("1 passed", "0 passed").as_bytes(), &names).is_err());
        assert!(outcomes(good.as_bytes(), &[format!("{PREFIX}different_test")]).is_err());
        let extra = format!(
            "test {CHILD} ... ok\ntest unexpected::control ... ok\ntest result: ok. 2 passed; 0 failed; 0 ignored; 0 measured;\n"
        );
        assert!(outcomes(extra.as_bytes(), &names).is_err());
        assert!(outcomes(good.replace("0 failed", "1 failed").as_bytes(), &names).is_err());
        assert!(fresh_marker(good.as_bytes(), MARKER.as_bytes()).is_ok());
        assert!(fresh_marker(good.as_bytes(), b"").is_err());
        assert!(fresh_marker(MARKER.as_bytes(), MARKER.as_bytes()).is_err());
    }
    #[test]
    fn supplier_binary_must_be_unique_libtest_and_native_only() {
        let row = json!({"reason":"compiler-artifact","package_id":"owned","target":{"name":"openssl","kind":["lib"]},"profile":{"test":true,"opt_level":"0","debuginfo":0},"features":["default","vendored"],"executable":"owned"});
        assert!(select_artifact(std::slice::from_ref(&row), "owned").is_ok());
        assert!(select_artifact(&[row.clone(), row.clone()], "owned").is_err());
        let mut wrong = row.clone();
        wrong["features"] = json!(["aws-lc"]);
        assert!(select_artifact(&[wrong], "owned").is_err());
        let mut ordinary = row;
        ordinary["profile"]["test"] = json!(false);
        assert!(select_artifact(&[ordinary], "owned").is_err());
    }
    #[test]
    fn standalone_graph_rejects_old_backend_extra_features_and_missing_dev_edge() {
        let scratch = Scratch::new("supplier-metadata-admission").unwrap();
        let root = &scratch.0;
        fs::create_dir_all(root.join("rust/vendor/openssl/src")).unwrap();
        fs::create_dir_all(root.join("rust/supplier-tests/openssl-native")).unwrap();
        fs::write(root.join("rust/vendor/openssl/src/lib.rs"), "// synthetic").unwrap();
        fs::write(root.join(MANIFEST), "# synthetic").unwrap();
        let expected: BTreeMap<String, Value> = serde_json::from_str(PACKAGES).unwrap();
        let ids: BTreeMap<String, String> = expected
            .keys()
            .map(|name| (name.clone(), format!("owned:{name}")))
            .collect();
        let mut packages = Vec::new();
        let mut nodes = Vec::new();
        for (name, row) in expected {
            let features = match name.as_str() {
                "openssl" => json!(["default", "vendored"]),
                "openssl-sys" => json!(["openssl-src", "vendored"]),
                "openssl-src" => json!(["default", "legacy"]),
                _ => json!([]),
            };
            packages.push(json!({"name":name,"id":ids[&name],"version":row["version"],"source":row["source"],
                "manifest_path":root.join(MANIFEST),"targets":[{"kind":["lib"],"src_path":root.join("rust/vendor/openssl/src/lib.rs")}]}));
            let dependencies: Vec<_> = row["dependencies"]
                .as_array()
                .unwrap()
                .iter()
                .map(|dep| ids[dep.as_str().unwrap()].clone())
                .collect();
            nodes.push(json!({"id":ids[&name],"dependencies":dependencies,"features":features}));
        }
        let good = json!({"packages":packages,"resolve":{"nodes":nodes},"workspace_members":[ids["openssl"]]});
        assert!(metadata(root, &good).is_ok());
        let mut old = good.clone();
        old["packages"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .find(|row| row["name"] == "openssl-src")
            .unwrap()["version"] = json!("300.6.0+3.6.2");
        assert!(metadata(root, &old).is_err());
        let mut backend = good.clone();
        backend["resolve"]["nodes"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .find(|row| row["id"] == ids["openssl-sys"])
            .unwrap()["features"] = json!(["aws-lc"]);
        assert!(metadata(root, &backend).is_err());
        let mut missing = good;
        missing["resolve"]["nodes"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .find(|row| row["id"] == ids["openssl"])
            .unwrap()["dependencies"]
            .as_array_mut()
            .unwrap()
            .retain(|value| value != &json!(ids["hex"]));
        assert!(metadata(root, &missing).is_err());
    }
}
