//! One explicit source+ready format. Old source/executable formats keep their policies.
use super::{Entry, ROOT, SOURCE, executable, pair, sha};
use crate::package::archive::{NATIVE, cpio, expanded_size};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

pub(super) const READY_KIND: &str = "native_benchmark_source_executable_ready";
const KIND: &str = "native_benchmark_source_executable_bundle";
const OUTER: &str = "bundle-manifest.json";
const INNER: &str = "source/native-source.cpio.gz";
const BIN: &str = "source-manifest.bin";
const JSON: &str = "source-manifest.json";
const FIXTURE: &str = "test/fixtures/ollama-routing.json";
const META: usize = 1024 * 1024;
const README: &[u8] = b"# Source+ready evaluator bundle\n\nOne artifact contains source/native-source.cpio.gz and a target-specific bin/autorouter-benchmark. Run evaluate-ollama --help without Node, Cargo, Git or installation; help makes no provider calls. Run extract-sources NEW_ABSOLUTE_DIRECTORY to write complete source files and canonical JSON inventory. Building sources needs the pinned toolchain and locked dependencies.\n\nThe fixture is synthetic. Binary provenance, platform qualification, release approval, quality and performance remain unverified. Evaluation needs an installed local model and predeclared thresholds; nothing is downloaded. No credentials, user configuration, private reports or caches are included.\n";

struct Admitted {
    files: BTreeMap<String, Entry>,
    source: BTreeMap<String, Entry>,
    charges: Value,
}
fn rows(files: &BTreeMap<String, Entry>) -> Value {
    json!(executable::declarations(files))
}
fn row(path: &str, e: &Entry) -> Value {
    json!({"path":path,"bytes":e.bytes.len(),"sha256":sha(&e.bytes),"mode":e.mode})
}
fn sum(files: &BTreeMap<String, Entry>) -> Result<usize, String> {
    files.values().try_fold(0usize, |n, e| {
        n.checked_add(e.bytes.len())
            .ok_or_else(|| "Complete source/executable sum overflow".into())
    })
}
fn size(facts: &Value, key: &str) -> Result<usize, String> {
    facts[key]
        .as_u64()
        .and_then(|n| usize::try_from(n).ok())
        .ok_or_else(|| "Invalid source size fact".into())
}
fn charge(
    outer: usize,
    expanded: usize,
    stored: usize,
    materialized: usize,
    inner: usize,
) -> Result<Value, String> {
    if stored > SOURCE.expanded
        || materialized > SOURCE.expanded
        || inner > SOURCE.expanded
        || expanded > NATIVE.expanded
    {
        return Err("Source or outer expansion exceeds its unchanged policy".into());
    }
    let add = |a: usize, b: usize| {
        a.checked_add(b)
            .filter(|n| *n <= NATIVE.expanded)
            .ok_or_else(|| "Complete source/executable data exceeds 64 MiB".to_owned())
    };
    Ok(json!({
        "stored_logical_bytes":add(outer, stored)?,
        "materialized_logical_bytes":add(outer, materialized)?,
        "composed_expanded_bytes":add(expanded, inner.max(materialized))?,
        "outer_tar_charge_bytes":expanded,"limit":NATIVE.expanded
    }))
}
pub(super) fn markers_present(root: &Path) -> Result<bool, String> {
    for name in [OUTER, INNER] {
        match fs::symlink_metadata(root.join(name)) {
            Ok(_) => return Ok(true),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return Err("Cannot inspect combined artifact marker".into()),
        }
    }
    Ok(false)
}
fn source(
    payload: &[u8],
    outer_payload: usize,
    outer_expanded: usize,
) -> Result<(BTreeMap<String, Entry>, Value), String> {
    let remaining = NATIVE
        .expanded
        .checked_sub(outer_payload.max(outer_expanded))
        .ok_or("Outer artifact exhausts composed source allowance")?;
    let policy = SOURCE.remaining(remaining);
    let decoded = cpio::binary_decode_root(payload, ROOT, policy, pair::source_limit)?;
    let mut files = decoded.files;
    let stored = sum(&files)?;
    let without_index = stored
        .checked_sub(files.get(BIN).ok_or("Missing source index")?.bytes.len())
        .ok_or("Invalid stored source sum")?;
    let allowed_json = policy
        .expanded
        .checked_sub(without_index)
        .ok_or("No remaining canonical source JSON allowance")?;
    let canonical = pair::canonical_source(&files, allowed_json)?;
    let binary = files.remove(BIN).ok_or("Missing stored source index")?;
    let binary_fact = row(BIN, &binary);
    files.insert(JSON.into(), super::data_entry(canonical));
    let materialized = sum(&files)?;
    if materialized > SOURCE.expanded {
        return Err("Materialized source exceeds 32 MiB".into());
    }
    // Apply the original privacy predicate to actual inner names as well.
    let forbidden = regex::Regex::new(
        r"(?:^|/)(?:\.env|\.git|node_modules|artifacts|config\.json|autorouter-session)",
    )
    .map_err(|_| "Cannot construct source privacy predicate")?;
    let exceptions: Vec<_> = files
        .keys()
        .filter(|p| forbidden.is_match(p))
        .map(String::as_str)
        .collect();
    if exceptions
        != [
            "rust/vendor/hyper-util/.github/workflows/CI.yml",
            "rust/vendor/hyper-util/.github/workflows/rustdoc-preview.yml",
            "rust/vendor/hyper-util/.gitignore",
        ]
        || exceptions.iter().any(|p| {
            !super::HYPER_UTIL_FILES
                .contains(&p.strip_prefix("rust/vendor/hyper-util/").unwrap_or(""))
        })
    {
        return Err("Inner source privacy inventory differs".into());
    }
    let facts = json!({
        "format":"gzip-binary-cpio-be","compressed_bytes":payload.len(),"sha256":sha(payload),
        "expanded_bytes":decoded.expanded_bytes,"stored_files":files.len(),
        "materialized_files":files.len(),"stored_payload_bytes":stored,
        "materialized_payload_bytes":materialized,"binary_manifest":binary_fact,
        "canonical_manifest":row(JSON, &files[JSON])
    });
    Ok((files, facts))
}
fn manifest(files: &BTreeMap<String, Entry>, facts: &Value, ready: &Value) -> Value {
    json!({
        "schema_version":1,"kind":KIND,"files":rows(files),"source":facts,
        "ready_kind":READY_KIND,"target":ready["target"],"fixture_sha256":ready["fixture_sha256"],
        "source_identity_sha256":ready["source_identity_sha256"],
        "source_to_binary_provenance":"unverified_prebuilt","private_material_included":false,
        "provider_calls":0,"release_approved":false
    })
}
fn admit(
    mut files: BTreeMap<String, Entry>,
    actual_expanded: Option<usize>,
) -> Result<Admitted, String> {
    let canonical_tar = expanded_size(&files, NATIVE)?;
    let outer_payload = sum(&files)?;
    let outer_charge = actual_expanded.unwrap_or(canonical_tar).max(canonical_tar);
    let outer = files
        .remove(OUTER)
        .ok_or("Missing combined artifact manifest")?;
    let payload = files
        .remove(INNER)
        .ok_or("Missing combined source payload")?;
    if outer.mode != 0o644
        || outer.bytes.len() > META
        || payload.mode != 0o644
        || payload.bytes.is_empty()
        || payload.bytes.len() > SOURCE.compressed
    {
        return Err("Invalid combined source or manifest bounds/mode".into());
    }
    let ready = executable::ready_record(&files, READY_KIND)?;
    pair::metadata::parse(&files[executable::MANIFEST].bytes, META)?;
    let (source, facts) = source(&payload.bytes, outer_payload, outer_charge)?;
    if ready["source_identity"] != rows(&source) || files[FIXTURE].bytes != source[FIXTURE].bytes {
        return Err("Ready identity differs from actual canonical source bytes".into());
    }
    files.insert(INNER.into(), payload);
    if pair::metadata::parse(&outer.bytes, META)? != manifest(&files, &facts, &ready) {
        return Err("Combined artifact declarations differ".into());
    }
    files.insert(OUTER.into(), outer);
    let charges = charge(
        sum(&files)?,
        outer_charge,
        size(&facts, "stored_payload_bytes")?,
        size(&facts, "materialized_payload_bytes")?,
        size(&facts, "expanded_bytes")?,
    )?;
    Ok(Admitted {
        files,
        source,
        charges,
    })
}
fn assemble(
    root: &Path,
    target: &str,
    binary: Vec<u8>,
    licenses: Vec<u8>,
    dependencies: Vec<Value>,
) -> Result<Admitted, String> {
    let native = pair::binary_source(root)?;
    let payload = cpio::binary_encode_root(ROOT, &native, SOURCE, pair::source_limit)?;
    // Producer prep is from its own just-encoded SOURCE32 tree; exact outer
    // remaining-budget admission below happens before any artifact is published.
    let (source, facts) = source(&payload, 0, 0)?;
    let mut files = executable::assemble_kind(
        root,
        target,
        binary,
        licenses,
        dependencies,
        READY_KIND,
        README,
    )?;
    let ready = executable::ready_record(&files, READY_KIND)?;
    if ready["source_identity"] != rows(&source) {
        return Err("Producer source changed between the complete input reads".into());
    }
    files.insert(INNER.into(), super::data_entry(payload));
    let outer = manifest(&files, &facts, &ready);
    files.insert(
        OUTER.into(),
        super::json_entry(&outer, "Cannot encode combined manifest")?,
    );
    admit(files, None)
}
pub(super) fn run(args: &[String], root: &Path) -> Result<bool, String> {
    let input = executable::inputs(args, root, "--source-executable")?;
    let destination = input.destination;
    let assembled = assemble(
        root,
        &input.target,
        input.binary,
        input.licenses,
        input.dependencies,
    )?;
    let (report, admitted) = super::write_archive_verified(
        ROOT,
        &assembled.files,
        &destination,
        NATIVE,
        |files, expanded| admit(files, Some(expanded)),
    )?;
    executable::print_report(
        report,
        &input.target,
        KIND,
        [
            ("complete_source_files", json!(admitted.source.len())),
            ("charges", admitted.charges),
        ],
    )
}
pub(super) fn receiver(
    root: &Path,
    mut files: BTreeMap<String, Entry>,
    args: &[String],
) -> Result<bool, String> {
    for (name, limit) in [(OUTER, META), (INNER, SOURCE.compressed)] {
        let (bytes, mode) = executable::read_file(&root.join(name), limit)?;
        files.insert(name.into(), Entry { bytes, mode });
    }
    let admitted = admit(files, None)?;
    let owner = pair::disk::Directory::open(root)?;
    let expected = super::file_bytes(&admitted.files);
    pair::disk::check_artifact_tree(&owner, &expected)?;
    if args.first().is_some_and(|a| a == "extract-sources") {
        if args.len() != 2 {
            return Err(
                "Usage: autorouter-benchmark extract-sources NEW_ABSOLUTE_DIRECTORY".into(),
            );
        }
        let destination = Path::new(&args[1]);
        if !destination.is_absolute() || destination.starts_with(root) {
            return Err("Source destination must be absolute and outside the bundle".into());
        }
        let output =
            pair::disk::create_tree(destination, &super::file_bytes(&admitted.source), || Ok(()))?;
        output.attached()?;
        owner.attached()?;
        pair::disk::check_artifact_tree(&owner, &expected)?;
        println!(
            "{}",
            json!({"kind":"native_benchmark_source_extraction","files":admitted.source.len(),
            "canonical_manifest":JSON,"source_bytes_modes_verified":true,"charges":admitted.charges,
            "provider_calls":0,"release_approved":false})
        );
        return Ok(true);
    }
    if args.is_empty() || matches!(args.first().map(String::as_str), Some("--help" | "help")) {
        println!(
            "Source+ready evaluator bundle: evaluate-ollama --help or extract-sources NEW_ABSOLUTE_DIRECTORY. No Node/Cargo/Git/install is required; help makes no provider calls."
        );
        return Ok(true);
    }
    let ready = admitted
        .files
        .into_iter()
        .filter(|(p, _)| p != OUTER && p != INNER)
        .collect();
    executable::dispatch(ready, args)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn canonical_full_source_bytes_order_headers_and_complete_bounds_are_preserved() {
        let malformed = [OUTER, INNER, "a", "b", "c", "d", "e", "f"]
            .into_iter()
            .map(|p| (p.into(), super::super::data_entry(vec![0])))
            .collect();
        assert!(admit(malformed, None).is_err());
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .canonicalize()
            .unwrap();
        let original = super::super::collect(&root).unwrap();
        let mut binary = pair::binary_source(&root).unwrap();
        assert_eq!(
            pair::canonical_source(&binary, META).unwrap(),
            original[JSON].bytes
        );
        assert!(pair::canonical_source(&binary, original[JSON].bytes.len() - 1).is_err());
        let payload = cpio::binary_encode_root(ROOT, &binary, SOURCE, pair::source_limit).unwrap();
        assert!(source(&payload, 0, NATIVE.expanded - 32).is_err());
        let materialized = source(&payload, 0, 0).unwrap().0;
        assert_eq!(materialized.len(), original.len());
        for (path, expected) in &original {
            assert_eq!(materialized[path].bytes, expected.bytes, "{path}");
            assert_eq!(materialized[path].mode, expected.mode, "{path}");
        }
        let paths = binary
            .keys()
            .filter(|p| p.as_str() != BIN)
            .map(String::as_str)
            .collect::<Vec<_>>();
        let mut altered = pair::metadata::decode_source(&binary[BIN].bytes, META, &paths).unwrap();
        altered["build_requires"] =
            json!("Different build instruction must not be normalized away");
        binary.get_mut(BIN).unwrap().bytes = pair::metadata::encode_source(&altered, META).unwrap();
        assert!(pair::source(&binary, 6).is_ok()); // Historical acceptance unchanged.
        assert!(pair::canonical_source(&binary, META).is_err());
        let mib = 1024 * 1024;
        let exact = charge(32 * mib, 32 * mib, 32 * mib, 32 * mib, 32 * mib).unwrap();
        assert_eq!(exact["composed_expanded_bytes"], 64 * mib);
        assert_eq!(exact["materialized_logical_bytes"], 64 * mib);
        assert!(charge(32 * mib + 1, 32 * mib, 32 * mib, 32 * mib, 32 * mib).is_err());
        assert!(charge(32 * mib, 32 * mib + 1, 32 * mib, 32 * mib, 32 * mib).is_err());
        assert!(charge(0, 0, 0, SOURCE.expanded + 1, 0).is_err());
        // Nonempty independent literal order oracle, not pretty(decoded).
        let map = BTreeMap::from([(FIXTURE.to_owned(), "a".repeat(64))]);
        let bytes = serde_json::to_vec_pretty(&super::super::source_record(&map)).unwrap();
        let expected = format!(
            concat!(
                "{{\n  \"schema_version\": 2,\n  \"kind\": \"native_benchmark_public_source_bundle\",\n",
                "  \"fixture_sha256\": \"{0}\",\n  \"files\": {{\n    \"test/fixtures/ollama-routing.json\": \"{0}\"\n  }},\n",
                "  \"build_requires\": \"Pinned Rust toolchain and locked dependencies via network or prepopulated Cargo cache; no Node required for native evaluator tools.\",\n",
                "  \"private_material_included\": false,\n  \"live_calls_performed\": false\n}}"
            ),
            "a".repeat(64)
        );
        assert_eq!(bytes, expected.as_bytes());
    }
}
