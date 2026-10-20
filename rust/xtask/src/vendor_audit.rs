//! Offline pinned-source audit. Git only reverses/reapplies patches in scratch.
//! The autorouter-audit binary builds without runtime-tools so its dependency
//! graph excludes the native sources inspected here; regular xtask can reuse it.
#[path = "nix_source.rs"]
pub(crate) mod nix_source;
#[path = "vendor_roots.rs"]
mod roots;
use crate::audit_support::{Scratch, digest, regular};
use crate::process::capture;
use regex::Regex;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Component, Path};
use std::process::Command;
use std::time::Duration;

const FILE_CAP: u64 = 16 * 1024 * 1024;
const TREE_CAP: usize = 64 * 1024 * 1024;
const MEMBER_CAP: usize = 1_024;
struct Spec {
    name: &'static str,
    version: &'static str,
    patch: &'static str,
    provenance_sha256: &'static str,
    upstream_count: usize,
    patched_count: usize,
    changed: &'static [&'static str],
    feature_dependencies: &'static [(&'static str, &'static [&'static str])],
    unidiff_zero: bool,
}
const SPECS: &[Spec] = &[
    Spec {
        name: "hyper",
        version: "1.12.0",
        patch: "hyper-node-http1-compat.patch",
        provenance_sha256: "fd02f8b688f7edcf0c091f4ed127ee748ff98b1bbdc16fdd28ff353a87771e54",
        upstream_count: 68,
        patched_count: 70,
        changed: &[
            "Cargo.toml",
            "Cargo.toml.orig",
            "src/ext/mod.rs",
            "src/proto/h1/conn.rs",
            "src/proto/h1/decode.rs",
            "src/proto/h1/dispatch.rs",
            "src/proto/h1/handoff_tests.rs",
            "src/proto/h1/io.rs",
            "src/proto/h1/mod.rs",
            "src/proto/h1/node_compat.rs",
            "src/proto/h1/role.rs",
            "src/server/conn/http1.rs",
        ],
        feature_dependencies: &[
            ("node-http1-body-handoff", &["node-http1-compat"]),
            ("node-http1-compat", &["http1", "server"]),
        ],
        unidiff_zero: false,
    },
    Spec {
        name: "hyper-util",
        version: "0.1.21",
        patch: "hyper-util-request-lease.patch",
        provenance_sha256: "0195bc375d47a28ec4474dd964041e5be25cbc25284bc03b1f3e2777adcb49ec",
        upstream_count: 62,
        patched_count: 63,
        changed: &[
            "Cargo.toml",
            "Cargo.toml.orig",
            "src/client/legacy/client.rs",
            "src/client/legacy/connect/mod.rs",
            "src/client/legacy/connect/request_lease.rs",
        ],
        feature_dependencies: &[
            ("node-http1-raw-pool", &["node-http1-request-lease"]),
            ("node-http1-request-lease", &["client-legacy", "http1"]),
        ],
        unidiff_zero: true,
    },
    Spec {
        name: "openssl",
        version: "0.10.81",
        patch: "openssl-node-trust.patch",
        provenance_sha256: "262e32bfff7e7c707d88ab2a9af86771ff90f5a76a0d13907b99686807b6b9b3",
        upstream_count: 112,
        patched_count: 114,
        changed: &[
            "src/error.rs",
            "src/lib.rs",
            "src/node_init.rs",
            "src/ssl/mod.rs",
            "src/ssl/node_defaults.rs",
            "src/ssl/test/mod.rs",
            "src/x509/mod.rs",
            "src/x509/store.rs",
        ],
        feature_dependencies: &[],
        unidiff_zero: false,
    },
];

const NIX: Spec = Spec {
    name: "nix",
    version: "0.31.3",
    patch: "nix-termios-init.patch",
    provenance_sha256: nix_source::PROVENANCE,
    upstream_count: 126,
    patched_count: 126,
    changed: &["src/sys/termios.rs"],
    feature_dependencies: &[],
    unidiff_zero: false,
};

fn hashes(value: &Value) -> Result<BTreeMap<String, String>, String> {
    value
        .as_object()
        .ok_or("Invalid pinned vendor file map")?
        .iter()
        .map(|(name, hash)| {
            if name.is_empty()
                || !Path::new(name)
                    .components()
                    .all(|part| matches!(part, Component::Normal(_)))
            {
                return Err("Invalid pinned vendor file path".into());
            }
            let hash = hash
                .as_str()
                .filter(|value| {
                    value.len() == 64
                        && value
                            .bytes()
                            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
                })
                .ok_or("Invalid pinned vendor file checksum")?;
            Ok((name.clone(), hash.to_owned()))
        })
        .collect()
}

fn inventory(root: &Path) -> Result<BTreeMap<String, String>, String> {
    if !fs::symlink_metadata(root)
        .map_err(|_| "Cannot inspect vendored root")?
        .is_dir()
    {
        return Err("Vendored root must be a directory, not a link".into());
    }
    let mut result = BTreeMap::new();
    let mut pending = vec![root.to_path_buf()];
    let mut total = 0usize;
    let mut members = 0usize;
    while let Some(directory) = pending.pop() {
        if directory
            .strip_prefix(root)
            .map_err(|_| "Invalid vendored path")?
            .components()
            .count()
            > 32
        {
            return Err("Vendored directory depth exceeds limit".into());
        }
        for entry in fs::read_dir(directory).map_err(|_| "Cannot enumerate vendored tree")? {
            let entry = entry.map_err(|_| "Cannot inspect vendored member")?;
            members = members
                .checked_add(1)
                .filter(|n| *n <= MEMBER_CAP)
                .ok_or("Vendored member count exceeds limit")?;
            let kind = entry
                .file_type()
                .map_err(|_| "Cannot inspect vendored member")?;
            if kind.is_dir() {
                pending.push(entry.path());
            } else {
                if !kind.is_file() {
                    return Err("Vendored tree contains a link or nonregular member".into());
                }
                if result.len() >= MEMBER_CAP {
                    return Err("Vendored member count exceeds limit".into());
                }
                let bytes = regular(&entry.path(), FILE_CAP.min((TREE_CAP - total) as u64))?;
                total = total
                    .checked_add(bytes.len())
                    .filter(|n| *n <= TREE_CAP)
                    .ok_or("Vendored tree byte count exceeds limit")?;
                let name = entry
                    .path()
                    .strip_prefix(root)
                    .map_err(|_| "Invalid vendored path")?
                    .to_str()
                    .ok_or("Vendored paths must be UTF-8")?
                    .to_owned();
                result.insert(name, digest(&bytes));
            }
        }
    }
    Ok(result)
}

fn feature_array(source: &str, key: &str) -> Result<Vec<String>, String> {
    let section = source
        .split_once("[features]\n")
        .ok_or("Missing vendored Cargo feature section")?
        .1;
    let section = Regex::new(r"(?m)^\[")
        .unwrap()
        .find(section)
        .map_or(section, |end| &section[..end.start()]);
    let pattern = Regex::new(&format!(
        r"(?m)^{}\s*=\s*\[([\s\S]*?)\]",
        regex::escape(key)
    ))
    .unwrap();
    let raw = pattern
        .captures(section)
        .and_then(|captures| captures.get(1))
        .ok_or("Missing vendored feature array")?
        .as_str();
    let strings = Regex::new(r#""([^"]*)""#).unwrap();
    let remainder = strings.replace_all(raw, "");
    if remainder.chars().any(|ch| !ch.is_whitespace() && ch != ',') {
        return Err("Invalid vendored feature array".into());
    }
    Ok(strings
        .captures_iter(raw)
        .map(|row| row[1].to_owned())
        .collect())
}

fn verify_features(source: &str, spec: &Spec) -> Result<(), String> {
    if spec.feature_dependencies.is_empty() {
        return Ok(());
    }
    for (name, expected) in spec.feature_dependencies {
        if feature_array(source, name)? != *expected {
            return Err("Vendored experimental feature dependencies changed".into());
        }
    }
    let full = feature_array(source, "full")?;
    if !feature_array(source, "default")?.is_empty()
        || full.iter().any(|name| {
            name == "node-http1-body-handoff"
                || name == "node-http1-request-lease"
                || name == "node-http1-raw-pool"
        })
    {
        return Err("Vendored experimental feature enabled by default/full".into());
    }
    Ok(())
}

fn copy_tree(from: &Path, to: &Path, members: &BTreeMap<String, String>) -> Result<(), String> {
    for (name, sha) in members {
        let bytes = regular(&from.join(name), FILE_CAP)?;
        if digest(&bytes) != *sha {
            return Err("Vendored file changed during verification".into());
        }
        let path = to.join(name);
        fs::create_dir_all(path.parent().ok_or("Invalid vendor scratch path")?)
            .map_err(|_| "Cannot create vendor scratch directory")?;
        fs::write(path, bytes).map_err(|_| "Cannot copy validated vendor scratch file")?;
    }
    Ok(())
}

fn apply(tree: &Path, patch: &Path, reverse: bool, spec: &Spec) -> Result<(), String> {
    let mut command = Command::new("git");
    command.args(["apply", "--unsafe-paths"]);
    if spec.unidiff_zero {
        command.arg("--unidiff-zero");
    }
    if reverse {
        command.arg("--reverse");
    }
    command.arg(patch).current_dir(tree);
    capture(&mut command, b"", Duration::from_secs(30))
        .map_err(|_| "Vendored patch reverse/forward verification failed")?;
    Ok(())
}

fn verify_tree(vendor: &Path, spec: &Spec) -> Result<Value, String> {
    let provenance = regular(
        &vendor.join(format!("{}-provenance.json", spec.name)),
        2 * 1024 * 1024,
    )?;
    if digest(&provenance) != spec.provenance_sha256 {
        return Err("Vendored provenance differs from its reviewed pin".into());
    }
    let manifest: Value =
        serde_json::from_slice(&provenance).map_err(|_| "Invalid vendor provenance JSON")?;
    let upstream = hashes(&manifest["upstream_files"])?;
    let patched = hashes(&manifest["patched_files"])?;
    if manifest["version"] != spec.version
        || upstream.len() != spec.upstream_count
        || patched.len() != spec.patched_count
    {
        return Err("Unexpected vendored provenance shape".into());
    }
    let changed = patched
        .iter()
        .filter(|(name, sha)| upstream.get(*name) != Some(*sha))
        .map(|(name, _)| name.as_str())
        .collect::<BTreeSet<_>>();
    let declared = manifest["changed_files"]
        .as_array()
        .ok_or("Missing changed vendor path declarations")?
        .iter()
        .map(|row| row.as_str().ok_or("Invalid vendor changed path"))
        .collect::<Result<BTreeSet<_>, _>>()?;
    if declared.len() != manifest["changed_files"].as_array().unwrap().len()
        || changed != declared
        || changed != spec.changed.iter().copied().collect()
    {
        return Err("Vendored patch exceeds its reviewed changed-file scope".into());
    }
    let source = vendor.join(spec.name);
    if inventory(&source)? != patched {
        return Err("Patched vendored file inventory differs".into());
    }
    for name in ["Cargo.toml", "Cargo.toml.orig"] {
        let bytes = regular(&source.join(name), FILE_CAP)?;
        verify_features(
            std::str::from_utf8(&bytes).map_err(|_| "Invalid Cargo manifest UTF-8")?,
            spec,
        )?;
    }
    let patch = regular(&vendor.join(spec.patch), FILE_CAP)?;
    if manifest["patch_sha256"] != digest(&patch) {
        return Err("Vendor patch checksum differs".into());
    }
    let scratch = Scratch::new("vendor-proof")?;
    let tree = scratch.0.join("tree");
    fs::create_dir(&tree).map_err(|_| "Cannot create vendor proof directory")?;
    copy_tree(&source, &tree, &patched)?;
    let patch = scratch.file("reviewed.patch", &patch)?;
    apply(&tree, &patch, true, spec)?;
    if inventory(&tree)? != upstream {
        return Err("Reconstructed upstream vendor inventory differs".into());
    }
    apply(&tree, &patch, false, spec)?;
    if inventory(&tree)? != patched {
        return Err("Reapplied vendor inventory differs".into());
    }
    Ok(
        json!({"passed":true,"name":spec.name,"version":spec.version,"upstream_files":upstream.len(),"patched_files":patched.len(),"changed_files":spec.changed,"provenance_sha256":digest(&provenance),"patch_sha256":digest(&regular(&vendor.join(spec.patch),FILE_CAP)?),"reverse_patch":true,"forward_patch":true,"experimental_features_default_off":(!spec.feature_dependencies.is_empty()).then_some(true)}),
    )
}

pub fn run(args: &[String], root: &Path) -> Result<bool, String> {
    if args == ["--help"] {
        println!(
            "Usage: cargo xtask verify-vendor [--only hyper|hyper-util|openssl|nix|node-ca]\nOffline pinned source/patch, feature, root certificate and license audit. No downloads, builds or execution of inspected source trees. For pre-build inspection use autorouter-audit with --no-default-features so inspected dependencies are excluded."
        );
        return Ok(true);
    }
    let selected = match args {
        [] => None,
        [flag, name] if flag == "--only" => Some(name.as_str()),
        _ => return Err("Expected verify-vendor [--only NAME]".into()),
    };
    if selected.is_some_and(|name| {
        !SPECS.iter().any(|spec| spec.name == name) && name != "node-ca" && name != "nix"
    }) {
        return Err("Unknown vendored audit component".into());
    }
    let vendor = root.join("rust/vendor");
    let mut reports = Vec::new();
    for spec in SPECS
        .iter()
        .filter(|spec| selected.is_none_or(|name| name == spec.name))
    {
        reports.push(verify_tree(&vendor, spec)?);
    }
    if selected.is_none_or(|name| name == "nix") {
        let suppliers = nix_source::resolve(root)?;
        let mut report = verify_tree(suppliers.vendor(), &NIX)?;
        let local = match fs::symlink_metadata(vendor.join("nix")) {
            Ok(_) => {
                verify_tree(&vendor, &NIX)?;
                true
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
            Err(_) => return Err("Cannot inspect local terminal supplier duplicate".into()),
        };
        report["cargo_suppliers"] = suppliers.identities();
        report["worktree_duplicate_present"] = json!(local);
        reports.push(report);
    }
    if selected.is_none_or(|name| name == "node-ca") {
        reports.push(roots::verify(&vendor.join("node-ca"))?);
    }
    println!("{}",serde_json::to_string_pretty(&json!({"schema_version":1,"kind":"native_vendor_audit","passed":true,"components":reports,"runtime_tools_feature":cfg!(feature="runtime-tools")})).unwrap());
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn actual_vendored_sources_round_trip_with_reviewed_licenses_and_feature_graphs() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        for spec in SPECS {
            assert_eq!(
                verify_tree(&root.join("rust/vendor"), spec).unwrap()["passed"],
                true
            );
        }
    }

    #[test]
    fn actual_git_terminal_supplier_round_trips_complete_reviewed_source() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .canonicalize()
            .unwrap();
        let suppliers = nix_source::resolve(&root).unwrap();
        let report = verify_tree(suppliers.vendor(), &NIX).unwrap();
        assert_eq!(report["passed"], true);
        assert_eq!(report["upstream_files"], 126);
        assert_eq!(report["patched_files"], 126);
        assert_eq!(report["changed_files"], json!(["src/sys/termios.rs"]));
        let scratch = Scratch::new("nix-specific-mutations").unwrap();
        let tree = scratch.0.join("nix");
        fs::create_dir(&tree).unwrap();
        copy_tree(
            &suppliers.vendor().join("nix"),
            &tree,
            &inventory(&suppliers.vendor().join("nix")).unwrap(),
        )
        .unwrap();
        for name in ["nix-provenance.json", "nix-termios-init.patch"] {
            fs::write(
                scratch.0.join(name),
                regular(&suppliers.vendor().join(name), FILE_CAP).unwrap(),
            )
            .unwrap();
        }
        assert_eq!(verify_tree(&scratch.0, &NIX).unwrap()["passed"], true);
        for name in ["src/sys/termios.rs", "LICENSE", ".cargo_vcs_info.json"] {
            let path = tree.join(name);
            let original = regular(&path, FILE_CAP).unwrap();
            let mut changed = original.clone();
            changed.extend(b"unreviewed mutation");
            fs::write(&path, changed).unwrap();
            assert!(verify_tree(&scratch.0, &NIX).is_err(), "{name}");
            fs::write(&path, &original).unwrap();
            fs::remove_file(&path).unwrap();
            assert!(verify_tree(&scratch.0, &NIX).is_err(), "missing:{name}");
            fs::write(&path, original).unwrap();
        }
        for name in ["nix-provenance.json", "nix-termios-init.patch"] {
            let path = scratch.0.join(name);
            let original = regular(&path, FILE_CAP).unwrap();
            fs::write(&path, b"PRIVATE_UNREVIEWED_METADATA").unwrap();
            let error = verify_tree(&scratch.0, &NIX).unwrap_err();
            assert!(!error.contains("PRIVATE_"));
            fs::write(&path, original).unwrap();
        }
        fs::write(tree.join("unreviewed"), b"extra supplier file").unwrap();
        assert!(verify_tree(&scratch.0, &NIX).is_err());
        fs::remove_file(tree.join("unreviewed")).unwrap();
        std::os::unix::fs::symlink("LICENSE", tree.join("link")).unwrap();
        assert!(verify_tree(&scratch.0, &NIX).is_err());
        fs::remove_file(tree.join("link")).unwrap();
        nix::unistd::mkfifo(&tree.join("fifo"), nix::sys::stat::Mode::S_IRUSR).unwrap();
        assert!(verify_tree(&scratch.0, &NIX).is_err());
    }

    #[test]
    fn tree_inventory_rejects_changed_missing_extra_links_and_nonregular_members() {
        let scratch = Scratch::new("vendor-inventory-mutations").unwrap();
        let tree = scratch.0.join("tree");
        fs::create_dir(&tree).unwrap();
        fs::write(tree.join("LICENSE"), b"synthetic license").unwrap();
        let expected = inventory(&tree).unwrap();
        fs::write(tree.join("LICENSE"), b"altered synthetic license").unwrap();
        assert_ne!(inventory(&tree).unwrap(), expected);
        fs::remove_file(tree.join("LICENSE")).unwrap();
        assert_ne!(inventory(&tree).unwrap(), expected);
        fs::write(tree.join("extra"), b"extra member").unwrap();
        assert_ne!(inventory(&tree).unwrap(), expected);
        std::os::unix::fs::symlink("extra", tree.join("link")).unwrap();
        assert!(inventory(&tree).is_err());
        fs::remove_file(tree.join("link")).unwrap();
        nix::unistd::mkfifo(&tree.join("fifo"), nix::sys::stat::Mode::S_IRUSR).unwrap();
        assert!(inventory(&tree).is_err());
    }

    #[test]
    fn feature_mutations_cannot_enable_experiments_or_change_their_dependencies() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        for spec in &SPECS[..2] {
            let original = fs::read_to_string(
                root.join("rust/vendor")
                    .join(spec.name)
                    .join("Cargo.toml.orig"),
            )
            .unwrap();
            assert!(verify_features(&original, spec).is_ok());
            for source in [
                original.replace("default = []", "default = [\"node-http1-body-handoff\"]"),
                original.replace("[features]", "[changed]"),
                original.replace(spec.feature_dependencies[0].1[0], "invalid-dependency"),
            ] {
                assert!(verify_features(&source, spec).is_err());
            }
        }
    }
}
