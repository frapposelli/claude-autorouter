//! Versioned paired carrier; only exact, explicitly versioned donor payloads are reused.
#[path = "bundle_transfer_fs.rs"]
pub(super) mod disk;
#[path = "bundle_transfer_json.rs"]
pub(super) mod metadata;
use super::{Entry, SOURCE, collect, sha};
use crate::package::archive::{DecodedArchive, cpio, decode_root, encode_root, expanded_size};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

// Versioned joint-transfer policy only. Individual archives keep SOURCE.
const SCHEMA7_JOINT_LIMIT: usize = 40 * 1024 * 1024;
fn joint_limit(schema: u64) -> Result<usize, String> {
    match schema {
        1..=6 => Ok(SOURCE.expanded),
        7 => Ok(SCHEMA7_JOINT_LIMIT),
        _ => Err("Unknown paired transfer schema".into()),
    }
}
fn reference_policy(
    schema: u64,
    native_expanded: usize,
) -> Result<crate::package::archive::ArchivePolicy, String> {
    // A joint budget must never enlarge either individual source archive.
    if native_expanded > SOURCE.expanded {
        return Err("Source exhausted paired transfer budget".into());
    }
    let remaining = joint_limit(schema)?
        .checked_sub(native_expanded)
        .ok_or("Source exhausted paired transfer budget")?;
    Ok(SOURCE.remaining(remaining))
}
fn carrier_total(schema: u64, native: usize, reference: usize) -> Result<usize, String> {
    if reference > reference_policy(schema, native)?.expanded {
        return Err("Reference exhausted paired transfer budget".into());
    }
    native.checked_add(reference).ok_or_else(|| "Paired transfer sum overflow".into())
}
fn schema_sum<'a>(
    files: impl IntoIterator<Item = &'a [u8]>,
    schema: u64,
) -> Result<usize, String> {
    let maximum = joint_limit(schema)?;
    if schema <= 6 {
        return sum(files);
    }
    files.into_iter().try_fold(0usize, |total, bytes| {
        total.checked_add(bytes.len()).filter(|total| *total <= maximum)
            .ok_or_else(|| "Complete logical transfer exceeds 40 MiB".into())
    })
}

const NATIVE_ROOT: &str = "autorouter-benchmark";
const CPIO_NAME: &str = "native-source.cpio.gz";
pub(super) fn source_limit(path: &str) -> usize {
    if super::LARGE_SYNTHETIC_CORPORA.contains(&path) {
        4 * FILE_LIMIT
    } else {
        FILE_LIMIT
    }
}
// Diagnostic arithmetic only: never used to admit/decode an oversized old tar.
fn legacy_full(
    native: &BTreeMap<String, Entry>,
    reference: usize,
    schema: u64,
) -> Result<usize, String> {
    let reference = if schema >= 4 {
        ["LICENSE", "rust/parity/baseline.json"]
            .into_iter()
            .try_fold(reference, |total, path| {
                let bytes = native
                    .get(path)
                    .ok_or("Missing legacy diagnostic donor")?
                    .bytes
                    .len();
                let padded = bytes
                    .checked_add(511)
                    .map(|n| n / 512 * 512)
                    .ok_or("Legacy full-tar size overflow")?;
                total
                    .checked_add(512)
                    .and_then(|n| n.checked_add(padded))
                    .ok_or("Legacy full-tar size overflow")
            })?
    } else {
        reference
    };
    native.values().try_fold(
        reference
            .checked_add(79360)
            .and_then(|n| n.checked_add(1024))
            .ok_or("Legacy full-tar size overflow")?,
        |total, entry| {
            let padded = entry
                .bytes
                .len()
                .checked_add(511)
                .map(|n| n / 512 * 512)
                .ok_or("Legacy full-tar size overflow")?;
            total
                .checked_add(512)
                .and_then(|n| n.checked_add(padded))
                .ok_or_else(|| "Legacy full-tar size overflow".into())
        },
    )
}
const DELTA_ROOT: &str = "autorouter-node-reference-delta";
const DELTA_NAME: &str = "frozen-node-reference-delta.tar.gz";
const META_LIMIT: usize = 64 * 1024;
const FILE_LIMIT: usize = 1024 * 1024;
/// Immutable fixture identity reviewed against the frozen baseline; shared by
/// the source/reference transfer and the executable evaluator handoff.
pub(super) const OLLAMA_ROUTING_FIXTURE: (&str, usize, &str) = (
    "test/fixtures/ollama-routing.json",
    17198,
    "1ee6ab593e47d87a67e922c0afb0c9db5787d8a27d1a0ff3e24baa0543291fc5",
);
const SHARED: &[(&str, usize, &str)] = &[
    (
        "test/fixtures/claude-protocol-v1.json",
        60523,
        "4cdaa7716255ec17032e26437f65c22aea170d8abb4d259c35c61e2709a1b41f",
    ),
    OLLAMA_ROUTING_FIXTURE,
];
const SHARED_LICENSE: (&str, usize, &str) = (
    "LICENSE",
    11358,
    "cfc7749b96f63bd31c3c42b5c471bf756814053e847c10f3eb003417bc523d30",
);
fn shared(schema: u64, baseline: &[u8]) -> Value {
    let mut rows: Vec<_> = SHARED
        .iter()
        .map(|(path, bytes, digest)| json!({"path":path,"bytes":bytes,"sha256":digest}))
        .collect();
    if schema >= 4 {
        let (path, bytes, digest) = SHARED_LICENSE;
        rows.push(json!({"path":path,"bytes":bytes,"sha256":digest}));
        rows.push(json!({"path":"frozen-reference-manifest.json","source_path":"rust/parity/baseline.json",
            "bytes":baseline.len(),"sha256":sha(baseline)}));
    }
    json!(rows)
}
fn donor(path: &str, schema: u64) -> Option<&str> {
    if schema >= 2 && SHARED.iter().any(|item| item.0 == path) {
        Some(path)
    } else if schema >= 4 && path == "LICENSE" {
        Some("LICENSE")
    } else if schema >= 4 && path == "frozen-reference-manifest.json" {
        Some("rust/parity/baseline.json")
    } else {
        None
    }
}
fn sum<'a>(files: impl IntoIterator<Item = &'a [u8]>) -> Result<usize, String> {
    files.into_iter().try_fold(0usize, |total, bytes| {
        total
            .checked_add(bytes.len())
            .filter(|total| *total <= SOURCE.expanded)
            .ok_or_else(|| "Complete logical transfer exceeds 32 MiB".into())
    })
}
pub(super) fn source(files: &BTreeMap<String, Entry>, schema: u64) -> Result<(), String> {
    let (name, foreign) = if schema >= 5 {
        ("source-manifest.bin", "source-manifest.json")
    } else {
        ("source-manifest.json", "source-manifest.bin")
    };
    if files.contains_key(foreign) {
        return Err("Native source manifest format differs from transfer schema".into());
    }
    let manifest = files.get(name).ok_or("Missing source manifest")?;
    let record = if schema >= 5 {
        let paths: Vec<_> = files
            .keys()
            .filter(|path| path.as_str() != name)
            .map(String::as_str)
            .collect();
        metadata::decode_source(&manifest.bytes, FILE_LIMIT, &paths)?
    } else {
        metadata::parse(&manifest.bytes, FILE_LIMIT)?
    };
    metadata::keys(
        &record,
        &[
            "schema_version",
            "kind",
            "fixture_sha256",
            "files",
            "build_requires",
            "private_material_included",
            "live_calls_performed",
        ],
    )?;
    if record["schema_version"].as_u64() != Some(2)
        || record["kind"] != "native_benchmark_public_source_bundle"
        || record["private_material_included"] != false
        || record["live_calls_performed"] != false
        || record["build_requires"].as_str().is_none()
    {
        return Err("Invalid native source manifest identity".into());
    }
    let declared = record["files"]
        .as_object()
        .ok_or("Missing native source file inventory")?;
    if declared.len().checked_add(1) != Some(files.len())
        || declared.contains_key("source-manifest.json")
        || declared.contains_key("source-manifest.bin")
    {
        return Err("Native source manifest has missing or foreign files".into());
    }
    for (path, file) in files {
        if path != name && (!super::allowed(path) && path != "README.native.md") {
            return Err("Foreign native source member".into());
        }
        let maximum = if super::LARGE_SYNTHETIC_CORPORA.contains(&path.as_str()) {
            4 * FILE_LIMIT
        } else {
            FILE_LIMIT
        };
        if file.mode != 0o644 || file.bytes.len() > maximum {
            return Err("Native source member violates mode or byte bounds".into());
        }
        if path != name
            && declared
                .get(path)
                .is_none_or(|hash| metadata::digest(hash).ok() != Some(sha(&file.bytes).as_str()))
        {
            return Err("Native source member differs from its historical manifest".into());
        }
    }
    let fixture = metadata::digest(&record["fixture_sha256"])?;
    if record["files"]["test/fixtures/ollama-routing.json"].as_str() != Some(fixture) {
        return Err("Native source fixture declaration differs".into());
    }
    Ok(())
}
fn logical<'a>(
    native: &'a BTreeMap<String, Entry>,
    reference: &'a BTreeMap<String, Entry>,
    baseline: &[u8],
    schema: u64,
) -> Result<disk::TreeFiles<'a>, String> {
    source(native, schema)?;
    if native
        .get("rust/parity/baseline.json")
        .is_none_or(|entry| entry.bytes != baseline)
        || (schema < 4
            && reference
                .get("frozen-reference-manifest.json")
                .is_none_or(|entry| entry.bytes != baseline))
    {
        return Err("Original frozen baseline manifest identity differs".into());
    }
    let record = metadata::parse(baseline, FILE_LIMIT)?;
    let entries = record["files"]
        .as_array()
        .filter(|rows| rows.len() == 111)
        .ok_or("Original reference must contain 111 frozen files")?;
    let mut expected = BTreeMap::new();
    for row in entries {
        let path = row["path"]
            .as_str()
            .filter(|path| super::safe(path))
            .ok_or("Unsafe frozen reference path")?;
        if expected.insert(path, row).is_some() {
            return Err("Duplicate original reference path".into());
        }
    }
    if expected.contains_key("LICENSE") || expected.contains_key("frozen-reference-manifest.json") {
        return Err("Unexpected original reference extras".into());
    }
    if schema >= 2 {
        for (path, bytes, hash) in SHARED {
            let entry = native
                .get(*path)
                .ok_or("Missing exact shared fixture donor")?;
            if entry.bytes.len() != *bytes
                || sha(&entry.bytes) != *hash
                || entry.mode != 0o644
                || reference.contains_key(*path)
            {
                return Err("Changed or duplicate exact shared fixture donor".into());
            }
            let original = expected
                .get(path)
                .ok_or("Shared donor is not an original fixture")?;
            if original["bytes"].as_u64() != Some(*bytes as u64) || original["sha256"] != *hash {
                return Err("Shared fixture differs from immutable reference".into());
            }
        }
    }
    if schema >= 4 {
        let (path, bytes, digest) = SHARED_LICENSE;
        let entry = native.get(path).ok_or("Missing pinned shared license")?;
        if entry.bytes.len() != bytes || sha(&entry.bytes) != digest || entry.mode != 0o644 {
            return Err("Shared license differs from pinned original bytes or mode".into());
        }
    }
    let mut files: disk::TreeFiles<'_> = BTreeMap::new();
    for path in expected
        .keys()
        .copied()
        .chain(["LICENSE", "frozen-reference-manifest.json"])
    {
        let entry = match donor(path, schema) {
            Some(path) => native.get(path),
            None => reference.get(path),
        }
        .ok_or("Reference payload is incomplete")?;
        if entry.mode != 0o644 || entry.bytes.len() > FILE_LIMIT {
            return Err("Reference member violates original mode or byte bounds".into());
        }
        if let Some(row) = expected.get(path)
            && (row["bytes"].as_u64() != Some(entry.bytes.len() as u64)
                || row["sha256"] != sha(&entry.bytes))
        {
            return Err("Original frozen reference bytes differ".into());
        }
        files.insert(path.into(), (entry.bytes.as_slice(), entry.mode));
    }
    let physical: BTreeSet<_> = files
        .keys()
        .filter(|path| donor(path, schema).is_none())
        .collect();
    if reference.keys().collect::<BTreeSet<_>>() != physical {
        return Err("Reference carrier contains foreign or missing members".into());
    }
    if files.get("LICENSE").is_none_or(|(bytes, _)| {
        native
            .get("LICENSE")
            .is_none_or(|license| license.bytes.as_slice() != *bytes)
    }) {
        return Err("Reference license is missing or different".into());
    }
    schema_sum(native
        .values()
        .map(|entry| entry.bytes.as_slice())
        .chain(files.values().map(|(bytes, _)| *bytes)), schema)?;
    Ok(files)
}
struct Plan {
    native: DecodedArchive,
    reference: DecodedArchive,
    baseline: Vec<u8>,
    schema: u64,
    record: Value,
}
fn load(root: &Path, directory: &Path) -> Result<Plan, String> {
    let input = disk::Directory::open(directory)?;
    let raw = input.read("transfer-manifest.json", META_LIMIT)?;
    let record = metadata::parse(&raw, META_LIMIT)?;
    let schema = record["schema_version"]
        .as_u64()
        .filter(|value| [1, 2, 3, 4, 5, 6, 7].contains(value))
        .ok_or("Unknown paired transfer schema")?;
    let joint_maximum = joint_limit(schema)?;
    let aggregate = if schema >= 3 {
        "aggregate_expanded_carrier_bytes"
    } else {
        "aggregate_expanded_tar_bytes"
    };
    let mut fields = vec![
        "schema_version",
        "kind",
        "baseline_commit",
        "baseline_version",
        "reference_files",
        "archives",
        aggregate,
        "aggregate_expanded_limit",
        "node_runtime_required",
        "native_build_required",
        "private_material_included",
        "binaries_included",
        "dependencies_bundled",
        "provider_calls",
        "release_approved",
        "performance_accepted",
    ];
    if schema >= 2 {
        fields.extend([
            "logical_reference_files",
            "reference_transport_files",
            "shared_inputs",
            "aggregate_logical_file_bytes",
            "aggregate_logical_file_limit",
            "legacy_full_tar_bytes",
            "legacy_full_tar_within_limit",
        ]);
    }
    metadata::keys(&record, &fields)?;
    for key in [
        "private_material_included",
        "binaries_included",
        "dependencies_bundled",
        "release_approved",
        "performance_accepted",
    ] {
        if record[key] != false {
            return Err("Unexpected transfer privacy or approval claim".into());
        }
    }
    if record["kind"] != "matched_native_benchmark_transfer"
        || record["provider_calls"].as_u64() != Some(0)
        || record["reference_files"].as_u64() != Some(111)
        || record["native_build_required"] != true
        || record["node_runtime_required"] != "22.14.0 for temporary reference tests only"
        || record["aggregate_expanded_limit"].as_u64() != Some(joint_maximum as u64)
    {
        return Err("Invalid paired transfer identity or limits".into());
    }
    // New packets must declare the fixed logical budget before archive reads.
    // Older schemas retain their existing validation and diagnostic order.
    if schema == 7
        && record["aggregate_logical_file_limit"].as_u64() != Some(joint_maximum as u64)
    {
        return Err("Logical paired transfer declaration differs".into());
    }
    let baseline = disk::Directory::open(root)?.read("rust/parity/baseline.json", FILE_LIMIT)?;
    let trusted = metadata::parse(&baseline, FILE_LIMIT)?;
    if record["baseline_commit"] != trusted["baseline_commit"]
        || record["baseline_version"] != trusted["baseline_version"]
    {
        return Err("Paired transfer frozen baseline differs".into());
    }
    let name = if schema >= 2 {
        DELTA_NAME
    } else {
        "frozen-node-reference.tar.gz"
    };
    let native_name = if schema >= 3 {
        CPIO_NAME
    } else {
        "native-source.tar.gz"
    };
    let expanded_key = if schema >= 3 {
        "expanded_carrier_bytes"
    } else {
        "expanded_tar_bytes"
    };
    metadata::keys(&record["archives"], &[native_name, name])?;
    let decode = |name: &str, archive_root: &str, policy| -> Result<DecodedArchive, String> {
        let fields = &record["archives"][name];
        metadata::keys(
            fields,
            if schema >= 3 {
                &["sha256", "expanded_carrier_bytes", "format"][..]
            } else {
                &["sha256", "expanded_tar_bytes"][..]
            },
        )?;
        if schema >= 3
            && fields["format"]
                != if name == CPIO_NAME {
                    if schema >= 6 {
                        "gzip-binary-cpio-be"
                    } else {
                        "gzip-newc"
                    }
                } else {
                    "gzip-ustar"
                }
        {
            return Err("Paired carrier format declaration differs".into());
        }
        let bytes = input.read(name, SOURCE.compressed)?;
        if metadata::digest(&fields["sha256"])? != sha(&bytes) {
            return Err("Paired transfer archive digest differs".into());
        }
        let archive = if schema >= 6 && name == CPIO_NAME {
            cpio::binary_decode_root(&bytes, archive_root, policy, source_limit)?
        } else if schema >= 3 && name == CPIO_NAME {
            cpio::decode_root(&bytes, archive_root, policy, source_limit)?
        } else {
            decode_root(&bytes, archive_root, policy)?
        };
        if archive.entries != archive.files.len() {
            return Err("Paired transfer archives require regular members only".into());
        }
        if fields[expanded_key].as_u64() != Some(archive.expanded_bytes as u64) {
            return Err("Paired transfer expansion declaration differs".into());
        }
        Ok(archive)
    };
    let native = decode(native_name, NATIVE_ROOT, SOURCE)?;
    let delta_policy = reference_policy(schema, native.expanded_bytes)?;
    let reference = decode(
        name,
        if schema >= 2 {
            DELTA_ROOT
        } else {
            "autorouter-node-reference"
        },
        delta_policy,
    )?;
    let stored = carrier_total(schema, native.expanded_bytes, reference.expanded_bytes)?;
    if record[aggregate].as_u64() != Some(stored as u64) {
        return Err("Paired transfer aggregate declaration differs".into());
    }
    let files = logical(&native.files, &reference.files, &baseline, schema)?;
    if schema >= 2 {
        let reused = &record["shared_inputs"];
        if reused != &shared(schema, &baseline) {
            return Err("Missing changed foreign or duplicate shared fixture declaration".into());
        }
        for row in reused
            .as_array()
            .ok_or("Invalid shared fixture declarations")?
        {
            metadata::keys(
                row,
                if schema >= 4 && row["path"] == "frozen-reference-manifest.json" {
                    &["path", "source_path", "bytes", "sha256"][..]
                } else {
                    &["path", "bytes", "sha256"][..]
                },
            )?;
        }
        let payload = schema_sum(native
            .files
            .values()
            .map(|entry| entry.bytes.as_slice())
            .chain(files.values().map(|(bytes, _)| *bytes)), schema)?;
        let legacy = if schema >= 3 {
            legacy_full(&native.files, reference.expanded_bytes, schema)?
        } else {
            stored
                .checked_add(79360)
                .ok_or("Legacy full-tar sum overflow")?
        };
        if record["logical_reference_files"].as_u64() != Some(113)
            || record["reference_transport_files"].as_u64()
                != Some(if schema >= 4 { 109 } else { 111 })
            || record["aggregate_logical_file_limit"].as_u64() != Some(joint_maximum as u64)
            || record["aggregate_logical_file_bytes"].as_u64() != Some(payload as u64)
            || record["legacy_full_tar_bytes"].as_u64() != Some(legacy as u64)
            || record["legacy_full_tar_within_limit"] != (legacy <= SOURCE.expanded)
        {
            return Err("Logical paired transfer declaration differs".into());
        }
    }
    let mut outer = raw.len();
    let resume = input.read("RESUME.md", META_LIMIT)?;
    let sums = input.read("SHA256SUMS", META_LIMIT)?;
    outer = outer
        .checked_add(resume.len())
        .and_then(|n| n.checked_add(sums.len()))
        .ok_or("Outer metadata overflow")?;
    let mut expected_sums = String::new();
    for archive in [native_name, name] {
        let hash = metadata::digest(&record["archives"][archive]["sha256"])?;
        let sidecar = input.read(&format!("{archive}.sha256"), META_LIMIT)?;
        let line = format!("{hash}  {archive}\n");
        if sidecar != line.as_bytes() {
            return Err("Transfer checksum sidecar differs".into());
        }
        outer = outer
            .checked_add(sidecar.len())
            .ok_or("Outer metadata overflow")?;
        expected_sums.push_str(&line);
    }
    expected_sums.push_str(&format!(
        "{}  transfer-manifest.json\n{}  RESUME.md\n",
        sha(&raw),
        sha(&resume)
    ));
    if outer > 4 * META_LIMIT || sums != expected_sums.as_bytes() {
        return Err("Transfer outer metadata bounds or checksums differ".into());
    }
    input.attached()?;
    Ok(Plan {
        native,
        reference,
        baseline,
        schema,
        record,
    })
}
pub(super) fn restore(root: &Path, directory: &Path, destination: &Path) -> Result<Value, String> {
    restore_with(root, directory, destination, || Ok(()), |_| Ok(()))
}
fn restore_with(
    root: &Path,
    directory: &Path,
    destination: &Path,
    mut checkpoint: impl FnMut() -> Result<(), String>,
    before_verify: impl FnOnce(&Path) -> Result<(), String>,
) -> Result<Value, String> {
    let plan = load(root, directory)?;
    let files = logical(
        &plan.native.files,
        &plan.reference.files,
        &plan.baseline,
        plan.schema,
    )?;
    checkpoint()?;
    let owned = disk::create_tree(destination, &files, &mut checkpoint)?;
    before_verify(&owned.path)?;
    owned.attached()?;
    if crate::reference::verify(root, &owned.path)? != 111 {
        return Err("Frozen reference verification count differs".into());
    }
    owned.attached()?;
    disk::check_tree(&owned, &files)?;
    checkpoint()?;
    owned.attached()?;
    Ok(
        json!({"schema_version":1,"kind":"paired_transfer_reference_restore","transfer_schema":plan.record["schema_version"],
        "reference_files":111,"logical_files":113,"original_modes_bytes_verified":true,"root_identity_verified":true,
        "shared_fixture_files":if plan.schema>=2{2}else{0},"license_restored":true,"provider_calls":0,"release_approved":false,"performance_accepted":false}),
    )
}

/// Preserve producer auto-freeze; receiver admission/restoration needs no Git.
fn reference(root: &Path) -> Result<(BTreeMap<String, Entry>, Vec<u8>), String> {
    let directory = root.join("artifacts/rust-rewrite/reference");
    crate::reference::freeze(root, &directory)?;
    let owner = disk::Directory::open(&directory)?;
    let original = disk::Directory::open(root)?;
    let baseline = original.read("rust/parity/baseline.json", FILE_LIMIT)?;
    let manifest = metadata::parse(&baseline, FILE_LIMIT)?;
    let rows = manifest["files"]
        .as_array()
        .filter(|rows| rows.len() == 111)
        .ok_or("Missing complete frozen reference")?;
    let mut files = BTreeMap::new();
    for row in rows {
        let path = row["path"]
            .as_str()
            .ok_or("Invalid original reference path")?;
        let bytes = owner.read(path, FILE_LIMIT)?;
        if row["bytes"].as_u64() != Some(bytes.len() as u64)
            || row["sha256"] != sha(&bytes)
            || files
                .insert(path.into(), super::data_entry(bytes))
                .is_some()
        {
            return Err("Frozen reference differs from its original inventory".into());
        }
    }
    files.insert(
        "frozen-reference-manifest.json".into(),
        super::data_entry(baseline.clone()),
    );
    files.insert(
        "LICENSE".into(),
        super::data_entry(original.read("LICENSE", FILE_LIMIT)?),
    );
    owner.attached()?;
    if crate::reference::verify(root, &owner.path)? != 111 {
        return Err("Incomplete frozen reference".into());
    }
    owner.attached()?;
    Ok((files, baseline))
}
pub(super) fn binary_source(root: &Path) -> Result<BTreeMap<String, Entry>, String> {
    let mut native = collect(root)?;
    // Only generated metadata changes representation; collect/default tar and
    // every raw source, fixture and mode retain their original bytes.
    source(&native, 4)?;
    let manifest = native
        .remove("source-manifest.json")
        .ok_or("Missing source manifest")?;
    let record = metadata::parse(&manifest.bytes, FILE_LIMIT)?;
    native.insert(
        "source-manifest.bin".into(),
        super::data_entry(metadata::encode_source(&record, FILE_LIMIT)?),
    );
    Ok(native)
}
// Combined and compact source kinds require original-order header identity.
pub(super) fn canonical_source(
    files: &BTreeMap<String, Entry>,
    maximum: usize,
) -> Result<Vec<u8>, String> {
    source(files, 6)?;
    let paths: Vec<_> = files
        .keys()
        .filter(|p| p.as_str() != "source-manifest.bin")
        .map(String::as_str)
        .collect();
    let decoded = metadata::decode_source(&files["source-manifest.bin"].bytes, FILE_LIMIT, &paths)?;
    canonical_record(&decoded, maximum)
}
pub(super) fn canonical_record(decoded: &Value, maximum: usize) -> Result<Vec<u8>, String> {
    let hashes: BTreeMap<String, String> = serde_json::from_value(decoded["files"].clone())
        .map_err(|_| "Invalid canonical source inventory")?;
    let record = super::source_record(&hashes);
    let expected = record
        .as_object()
        .ok_or("Invalid canonical source record")?;
    let actual = decoded.as_object().ok_or("Invalid decoded source record")?;
    if actual.len() != expected.len()
        || expected
            .iter()
            .any(|(key, value)| actual.get(key) != Some(value))
    {
        return Err("Source header differs from original canonical identity".into());
    }
    // Fixed-capacity standard writer cannot allocate past remaining materialized bytes.
    let mut bytes = vec![0; maximum.min(FILE_LIMIT)];
    let length = {
        let mut output = std::io::Cursor::new(bytes.as_mut_slice());
        serde_json::to_writer_pretty(&mut output, &record)
            .map_err(|_| "Canonical source JSON exceeds remaining allowance")?;
        usize::try_from(output.position()).map_err(|_| "Canonical source length overflow")?
    };
    bytes.truncate(length);
    Ok(bytes)
}
pub(super) fn create(root: &Path, destination: &Path) -> Result<Value, String> {
    create_schema(root, destination, 7)
}
pub(super) fn create_schema6(root: &Path, destination: &Path) -> Result<Value, String> {
    create_schema(root, destination, 6)
}
pub(super) fn create_schema7(root: &Path, destination: &Path) -> Result<Value, String> {
    create_schema(root, destination, 7)
}
fn create_schema(root: &Path, destination: &Path, schema: u64) -> Result<Value, String> {
    if !matches!(schema, 6 | 7) {
        return Err("Unsupported paired transfer producer schema".into());
    }
    let joint_maximum = joint_limit(schema)?;
    let native = binary_source(root)?;
    let (mut delta, baseline) = reference(root)?;
    for (path, _, _) in SHARED {
        delta
            .remove(*path)
            .ok_or("Missing original shared fixture")?;
    }
    for path in ["LICENSE", "frozen-reference-manifest.json"] {
        delta.remove(path).ok_or("Missing original shared extra")?;
    }
    let files = logical(&native, &delta, &baseline, schema)?;
    let payload = schema_sum(native
        .values()
        .map(|entry| entry.bytes.as_slice())
        .chain(files.values().map(|(bytes, _)| *bytes)), schema)?;
    let native_expanded = cpio::binary_expanded_size(NATIVE_ROOT, &native, SOURCE, source_limit)?;
    let delta_policy = reference_policy(schema, native_expanded)?;
    let delta_expanded = expanded_size(&delta, delta_policy)?;
    let archive = |name: &str,
                   files: &BTreeMap<String, Entry>,
                   policy: crate::package::archive::ArchivePolicy|
     -> Result<Vec<u8>, String> {
        let bytes = encode_root(name, files, policy)?;
        let decoded = decode_root(&bytes, name, policy)?;
        if decoded.files.len() != files.len()
            || files.iter().any(|(path, entry)| {
                decoded
                    .files
                    .get(path)
                    .is_none_or(|found| found.bytes != entry.bytes || found.mode != entry.mode)
            })
        {
            return Err("Paired archive readback differs".into());
        }
        Ok(bytes)
    };
    let native_bytes = cpio::binary_encode_root(NATIVE_ROOT, &native, SOURCE, source_limit)?;
    let verified = cpio::binary_decode_root(&native_bytes, NATIVE_ROOT, SOURCE, source_limit)?;
    if verified.expanded_bytes != native_expanded
        || verified.files.len() != native.len()
        || native.iter().any(|(path, entry)| {
            verified
                .files
                .get(path)
                .is_none_or(|found| found.bytes != entry.bytes || found.mode != entry.mode)
        })
    {
        return Err("Paired native CPIO readback differs".into());
    }
    let delta_bytes = archive(DELTA_ROOT, &delta, delta_policy)?;
    let stored = carrier_total(schema, native_expanded, delta_expanded)?;
    let legacy = legacy_full(&native, delta_expanded, schema)?;
    let delta_baseline = baseline;
    let baseline = metadata::parse(&delta_baseline, FILE_LIMIT)?;
    let record = json!({"schema_version":schema,"kind":"matched_native_benchmark_transfer",
        "baseline_commit":baseline["baseline_commit"],"baseline_version":baseline["baseline_version"],"reference_files":111,
        "archives":{(CPIO_NAME):{"sha256":sha(&native_bytes),"expanded_carrier_bytes":native_expanded,"format":"gzip-binary-cpio-be"},
            (DELTA_NAME):{"sha256":sha(&delta_bytes),"expanded_carrier_bytes":delta_expanded,"format":"gzip-ustar"}},
        "aggregate_expanded_carrier_bytes":stored,"aggregate_expanded_limit":joint_maximum,
        "node_runtime_required":"22.14.0 for temporary reference tests only","native_build_required":true,
        "private_material_included":false,"binaries_included":false,"dependencies_bundled":false,"provider_calls":0,
        "release_approved":false,"performance_accepted":false,"logical_reference_files":113,"reference_transport_files":109,
        "shared_inputs":shared(schema, &delta_baseline),"aggregate_logical_file_bytes":payload,"aggregate_logical_file_limit":joint_maximum,
        "legacy_full_tar_bytes":legacy,"legacy_full_tar_within_limit":legacy<=SOURCE.expanded});
    let raw = serde_json::to_vec_pretty(&record).map_err(|_| "Cannot encode transfer metadata")?;
    let resume_text = super::transfer::readme(schema);
    let resume = resume_text.as_bytes();
    let native_sum = format!("{}  {CPIO_NAME}\n", sha(&native_bytes));
    let delta_sum = format!("{}  {DELTA_NAME}\n", sha(&delta_bytes));
    let sums = format!(
        "{native_sum}{delta_sum}{}  transfer-manifest.json\n{}  RESUME.md\n",
        sha(&raw),
        sha(resume)
    );
    let outer = [
        &raw[..],
        resume,
        native_sum.as_bytes(),
        delta_sum.as_bytes(),
        sums.as_bytes(),
    ];
    if outer.iter().any(|bytes| bytes.len() > META_LIMIT)
        || outer.iter().map(|bytes| bytes.len()).sum::<usize>() > 4 * META_LIMIT
    {
        return Err("Outer transfer metadata exceeds declared bounds".into());
    }
    let parent = disk::Directory::open(destination.parent().ok_or("Missing transfer parent")?)?;
    let owned = parent.create(
        destination
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or("Invalid transfer destination")?,
    )?;
    let carriers = [
        (CPIO_NAME, native_bytes.as_slice(), 0o644),
        (DELTA_NAME, delta_bytes.as_slice(), 0o644),
        ("transfer-manifest.json", raw.as_slice(), 0o644),
        ("RESUME.md", resume, 0o644),
        ("native-source.cpio.gz.sha256", native_sum.as_bytes(), 0o644),
        (
            "frozen-node-reference-delta.tar.gz.sha256",
            delta_sum.as_bytes(),
            0o644,
        ),
        ("SHA256SUMS", sums.as_bytes(), 0o644),
    ];
    disk::write_tree(&owned, carriers, || Ok(()))?;
    let expected = carriers
        .iter()
        .map(|(path, bytes, mode)| ((*path).to_owned(), (*bytes, *mode)))
        .collect();
    disk::check_tree(&owned, &expected)?;
    // Same ingress and full logical checks as a receiver, before publication.
    load(root, &owned.path)?;
    owned.attached()?;
    Ok(record)
}
#[cfg(test)]
#[path = "bundle_transfer_pair_tests.rs"]
mod tests;
