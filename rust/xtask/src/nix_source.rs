//! Resolve the reviewed production supplier without compiling or linking it.
use crate::audit_support::{digest, regular};
use crate::process::capture;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

pub const PROVENANCE: &str = "3d67145efc23e48c3ea1aff66b37148a21499d3df941ebb70bb3434902e35dec";
pub const ARCHIVE: &str = "cf20d2fde8ff38632c426f1165ed7436270b44f199fc55284c38276f9db47c3d";
pub const LICENSE: &str = "66e3ee1fa7f909ad3c612d556f2a0cdabcd809ad6e66f3b0605015ac64841b70";
const REGISTRY: &str = "registry+https://github.com/rust-lang/crates.io-index";
// These opaque IDs are actual Cargo 1.99 receipts, not inferred URL formats.
const REGISTRY_ID: &str = "registry+https://github.com/rust-lang/crates.io-index#nix@0.31.3";
const GIT_ID: &str = "git+https://github.com/frapposelli/claude-autorouter.git?rev=2286fd91d8165b4b56b273c2ebe5fb4f462ed672#nix@0.31.3";
const METADATA_CAP: usize = 4 * 1024 * 1024;

fn text<'a>(row: &'a Value, field: &str) -> Result<&'a str, String> {
    row[field]
        .as_str()
        .ok_or_else(|| "Invalid supplier metadata".into())
}
#[derive(Clone)]
pub struct Package {
    pub id: String,
    pub manifest: PathBuf,
    pub source: String,
    name: String,
}
pub struct Suppliers {
    pub production: Package,
    pub bootstrap: Package,
    packages: BTreeMap<String, Package>,
}
#[derive(Clone, Copy)]
pub enum Role {
    Production,
    Bootstrap,
}

fn pinned_source(root: &Path) -> Result<String, String> {
    let bytes = regular(&root.join("rust/vendor/nix-source.json"), 4096)?;
    if bytes.as_slice() != include_bytes!("../../vendor/nix-source.json").as_slice() {
        return Err("Supplier descriptor differs from compiled review".into());
    }
    let pin: Value = serde_json::from_slice(&bytes).map_err(|_| "Invalid supplier descriptor")?;
    let fields = pin.as_object().ok_or("Invalid supplier descriptor")?;
    if fields.keys().map(String::as_str).collect::<BTreeSet<_>>()
        != ["schema_version", "source", "provenance_sha256"]
            .into_iter()
            .collect()
        || pin["schema_version"] != 1
        || pin["provenance_sha256"] != PROVENANCE
    {
        return Err("Supplier descriptor exceeds reviewed scope".into());
    }
    Ok(text(&pin, "source")?.to_owned())
}

// Cargo 1.99 generates these canonical quoted fields. Reject other syntax;
// this is not a general TOML parser or a representation-compatibility claim.
fn locked_sources(lock: &str, git: &str) -> Result<(), String> {
    let mut found = BTreeSet::new();
    for block in lock.split("[[package]]").skip(1) {
        let field = |name: &str| -> Result<Option<String>, String> {
            let values = block
                .lines()
                .filter_map(|line| line.split_once('='))
                .filter(|(key, _)| key.trim() == name)
                .map(|(_, value)| value.trim())
                .collect::<Vec<_>>();
            if values.len() > 1 {
                return Err("Duplicate locked supplier field".into());
            }
            values
                .first()
                .map(|value| {
                    serde_json::from_str::<String>(value)
                        .map_err(|_| "Noncanonical locked supplier field".into())
                })
                .transpose()
        };
        if field("name")?.as_deref() != Some("nix") {
            continue;
        }
        let source = field("source")?.ok_or("Missing locked supplier source")?;
        if field("version")?.as_deref() != Some("0.31.3")
            || (source != git && source != REGISTRY)
            || !found.insert(source.clone())
            || if source == REGISTRY {
                field("checksum")?.as_deref() != Some(ARCHIVE)
            } else {
                field("checksum")?.is_some()
            }
        {
            return Err("Locked terminal supplier identity differs".into());
        }
    }
    if found.len() != 2 {
        return Err("Locked terminal suppliers are not unique".into());
    }
    Ok(())
}

fn admit(metadata: &Value, lock: &str, git: &str, root: &Path) -> Result<Suppliers, String> {
    locked_sources(lock, git)?;
    if metadata["version"] != 1 || metadata["workspace_root"].as_str() != root.join("rust").to_str()
    {
        return Err("Supplier metadata workspace or format differs".into());
    }
    let rows = metadata["packages"]
        .as_array()
        .filter(|rows| rows.len() <= 1024)
        .ok_or("Invalid supplier package list")?;
    let mut packages = BTreeMap::new();
    let mut production = None;
    let mut bootstrap = None;
    for row in rows {
        let name = text(row, "name")?.to_owned();
        let id = text(row, "id")?.to_owned();
        let source = match row.get("source") {
            Some(Value::Null) => String::new(),
            Some(Value::String(source)) => source.clone(),
            _ => return Err("Invalid package source identity".into()),
        };
        let package = Package {
            id: id.clone(),
            manifest: PathBuf::from(text(row, "manifest_path")?),
            source,
            name: name.clone(),
        };
        if id.is_empty() || id.len() > 4096 || packages.insert(id, package.clone()).is_some() {
            return Err("Supplier package identities are not unique".into());
        }
        if name != "nix" {
            continue;
        }
        if text(row, "version")? != "0.31.3"
            || !package.manifest.is_absolute()
            || package.id
                != if package.source == git {
                    GIT_ID
                } else {
                    REGISTRY_ID
                }
        {
            return Err("Terminal supplier version or path differs".into());
        }
        if package.source == git && production.replace(package.clone()).is_none() {
            if !package.manifest.ends_with("rust/vendor/nix/Cargo.toml") {
                return Err("Git supplier manifest is outside reviewed layout".into());
            }
        } else if package.source == REGISTRY && bootstrap.replace(package).is_none() {
        } else {
            return Err("Unexpected or duplicate terminal supplier source".into());
        }
    }
    let production = production.ok_or("Missing production terminal supplier")?;
    let bootstrap = bootstrap.ok_or("Missing bootstrap terminal supplier")?;
    let nodes = metadata["resolve"]["nodes"]
        .as_array()
        .filter(|nodes| nodes.len() <= 1024)
        .ok_or("Missing resolved supplier graph")?;
    let node = |id: &str| -> Result<&Value, String> {
        let matching = nodes
            .iter()
            .filter(|node| node["id"] == id)
            .collect::<Vec<_>>();
        if matching.len() != 1 {
            return Err("Supplier graph node is not unique".into());
        }
        Ok(matching[0])
    };
    for (package, expected) in [
        (
            &production,
            &[
                "event", "feature", "fs", "poll", "process", "signal", "term", "user",
            ][..],
        ),
        (
            &bootstrap,
            &[
                "event", "feature", "fs", "poll", "process", "resource", "signal", "user",
            ][..],
        ),
    ] {
        let features = node(&package.id)?["features"]
            .as_array()
            .ok_or("Invalid resolved supplier features")?;
        if features
            .iter()
            .map(|value| value.as_str().ok_or("Invalid supplier feature"))
            .collect::<Result<BTreeSet<_>, _>>()?
            != expected.iter().copied().collect()
            || features.len() != expected.len()
        {
            return Err("Resolved terminal supplier feature graph differs".into());
        }
    }
    for (relative, expected) in [
        ("crates/autorouter-runtime", &production),
        ("crates/claude-autorouter", &production),
        ("xtask", &bootstrap),
    ] {
        let manifest = root.join("rust").join(relative).join("Cargo.toml");
        let owners = packages
            .values()
            .filter(|package| package.manifest == manifest)
            .collect::<Vec<_>>();
        if owners.len() != 1
            || owners[0].name != relative.rsplit('/').next().unwrap()
            || !owners[0].source.is_empty()
        {
            return Err("Supplier graph owner is not unique".into());
        }
        let edges = node(&owners[0].id)?["deps"]
            .as_array()
            .ok_or("Missing supplier dependency edges")?;
        let selected = edges
            .iter()
            .filter(|edge| edge["name"] == "nix")
            .collect::<Vec<_>>();
        if selected.len() != 1
            || selected[0]["pkg"] != expected.id
            || !selected[0]["dep_kinds"].as_array().is_some_and(|kinds| {
                kinds.iter().any(|kind| {
                    kind.as_object().is_some_and(|kind| {
                        kind.get("kind") == Some(&Value::Null)
                            && kind.get("target") == Some(&Value::Null)
                    })
                })
            })
        {
            return Err("Resolved terminal supplier edge differs".into());
        }
    }
    Ok(Suppliers {
        production,
        bootstrap,
        packages,
    })
}

#[cfg(test)]
#[path = "nix_source_tests.rs"]
mod tests;

fn from_metadata(root: &Path, metadata: &Value) -> Result<Suppliers, String> {
    let source = pinned_source(root)?;
    let lock = regular(&root.join("rust/Cargo.lock"), METADATA_CAP as u64)?;
    let lock = std::str::from_utf8(&lock).map_err(|_| "Invalid locked supplier UTF-8")?;
    let suppliers = admit(metadata, lock, &source, root)?;
    for package in [&suppliers.production, &suppliers.bootstrap] {
        if package
            .manifest
            .canonicalize()
            .map_err(|_| "Cannot resolve supplier manifest")?
            != package.manifest
            || digest(&regular(&package.manifest, 1024 * 1024)?)
                != "36654c48d8a7941649b643d42b15750a0dd024095f8670b3e928c503339eefe6"
            || digest(&regular(
                &package.manifest.parent().unwrap().join("LICENSE"),
                1024 * 1024,
            )?) != LICENSE
        {
            return Err("Supplier manifest differs from reviewed source".into());
        }
    }
    Ok(suppliers)
}

pub fn resolve(root: &Path) -> Result<Suppliers, String> {
    let bytes = capture(
        Command::new("cargo")
            .args(["metadata", "--format-version", "1", "--locked", "--offline"])
            .current_dir(root.join("rust")),
        b"",
        Duration::from_secs(60),
    )
    .map_err(|_| "Cannot resolve locked offline terminal suppliers")?;
    let (_, suppliers) = from_bytes(root, &bytes)?;
    Ok(suppliers)
}
fn parse_metadata(bytes: &[u8]) -> Result<Value, String> {
    if bytes.len() > METADATA_CAP {
        return Err("Supplier metadata exceeds admission limit".into());
    }
    serde_json::from_slice(bytes).map_err(|_| "Invalid supplier metadata JSON".into())
}
pub fn from_bytes(root: &Path, bytes: &[u8]) -> Result<(Value, Suppliers), String> {
    let metadata = parse_metadata(bytes)?;
    let suppliers = from_metadata(root, &metadata)?;
    Ok((metadata, suppliers))
}

impl Suppliers {
    pub fn vendor(&self) -> &Path {
        self.production.manifest.parent().unwrap().parent().unwrap()
    }
    pub fn library(&self, bytes: &[u8], role: Role) -> Result<Value, String> {
        let expected = match role {
            Role::Production => &self.production,
            Role::Bootstrap => &self.bootstrap,
        };
        if bytes.len() > 64 * 1024 * 1024 {
            return Err("Supplier build report exceeds byte limit".into());
        }
        let body = std::str::from_utf8(bytes).map_err(|_| "Invalid supplier build report UTF-8")?;
        if body.split('\n').count() > 100_000 {
            return Err("Supplier build report exceeds record limit".into());
        }
        let mut libraries = Vec::new();
        for line in body.split('\n').filter(|line| !line.trim().is_empty()) {
            if line.len() > 2 * 1024 * 1024 {
                return Err("Supplier artifact record exceeds byte limit".into());
            }
            let row: Value =
                serde_json::from_str(line).map_err(|_| "Invalid supplier artifact record")?;
            if row["reason"] != "compiler-artifact" {
                continue;
            }
            let package = self
                .packages
                .get(text(&row, "package_id")?)
                .ok_or("Artifact package is absent from locked metadata")?;
            if row["manifest_path"].as_str() != package.manifest.to_str() {
                return Err("Artifact manifest differs from locked metadata".into());
            }
            if package.name != "nix" {
                if row["target"]["name"] == "nix" {
                    return Err("Foreign package exports a terminal supplier target".into());
                }
                continue;
            }
            if package.id != expected.id {
                return Err("Build compiled the wrong terminal supplier".into());
            }
            let features = row["features"]
                .as_array()
                .ok_or("Invalid supplier artifact features")?;
            let allowed: &[&str] = match role {
                Role::Production => &["feature", "fs", "process", "signal", "term", "user"],
                Role::Bootstrap => &[
                    "event", "feature", "fs", "poll", "process", "resource", "signal", "user",
                ],
            };
            if features
                .iter()
                .map(|value| value.as_str().ok_or("Invalid supplier feature"))
                .collect::<Result<BTreeSet<_>, _>>()?
                != allowed.iter().copied().collect()
                || features.len() != allowed.len()
            {
                return Err("Supplier artifact feature graph differs".into());
            }
            let (kind, crate_type, source) = match row["target"]["name"].as_str() {
                Some("nix") => ("lib", "lib", "src/lib.rs"),
                Some("build-script-build") => ("custom-build", "bin", "build.rs"),
                _ => return Err("Unsupported terminal supplier target".into()),
            };
            if row["target"]["kind"] != json!([kind])
                || row["target"]["crate_types"] != json!([crate_type])
                || row["target"]["edition"] != "2021"
                || row["target"]["src_path"].as_str()
                    != package.manifest.parent().unwrap().join(source).to_str()
                || row["profile"]["test"] != false
            {
                return Err("Terminal supplier target differs from reviewed source".into());
            }
            if kind == "lib" {
                if matches!(role, Role::Production)
                    && (row["profile"]["test"] != false
                        || row["profile"]["opt_level"] != "3"
                        || row["profile"]["debug_assertions"] != false)
                {
                    return Err("Terminal supplier is not an ordinary release artifact".into());
                }
                libraries.push(row);
            }
        }
        if libraries.len() != 1 {
            return Err("Build lacks a unique terminal supplier library".into());
        }
        Ok(
            json!({"package_id":expected.id,"manifest_path":expected.manifest,"source":expected.source,"features":libraries[0]["features"],"role":match role {Role::Production=>"production-patched",Role::Bootstrap=>"bootstrap-registry"}}),
        )
    }
    pub fn identities(&self) -> Value {
        json!({"production":{"package_id":self.production.id,"manifest_path":self.production.manifest,"source":self.production.source},"bootstrap":{"package_id":self.bootstrap.id,"manifest_path":self.bootstrap.manifest,"source":self.bootstrap.source}})
    }
}
