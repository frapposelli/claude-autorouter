//! Explicit temporary-reference handoff, separate from default Rust-only bundle.
use super::{Entry, SOURCE, collect, regular, sha, write_archive};
use crate::package::archive::expanded_size;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Component, Path};

const REFERENCE_ROOT: &str = "autorouter-node-reference";
const README: &str = r#"# Matched Rust rewrite benchmark transfer

This directory contains public source and synthetic fixtures plus the exact
temporary Node compatibility reference. It contains no built executable, Cargo
cache, credentials, user configuration, real transcripts or private reports.
The reference is only for migration comparisons and is not a product sidecar.
This transfer does not establish release approval or performance acceptance.

Verify before unpacking:

    shasum -a 256 -c SHA256SUMS
    tar -xzf native-source.tar.gz
    tar -xzf frozen-node-reference.tar.gz
    mkdir -p autorouter-benchmark/artifacts/rust-rewrite
    mv autorouter-node-reference autorouter-benchmark/artifacts/rust-rewrite/reference
    cd autorouter-benchmark/rust

No Git checkout is needed: the driver verifies every supplied reference file
against rust/parity/baseline.json before importing it. Install the pinned Rust
toolchain from rust-toolchain.toml, Node22.14.0, and C compiler/Make/Perl build
prerequisites. Cargo.lock dependencies require network access or an existing
Cargo cache; no evaluator or model is downloaded by this transfer.

In a dedicated Bash/Zsh shell set the shared descriptor condition. Build and
audit the ordinary CLI separately from the development benchmark helper. A
combined Cargo invocation would unify the helper's development probes into the
CLI. From the extracted rust directory, use a fresh evidence directory:

    ulimit -n 1024
    mkdir ../artifacts/rust-rewrite/transfer-build-1
    cargo fetch --locked
    cargo build --locked --package xtask --no-default-features --bin autorouter-audit --message-format=json > ../artifacts/rust-rewrite/transfer-build-1/bootstrap-build.jsonl
    cp target/debug/autorouter-audit ../artifacts/rust-rewrite/transfer-build-1/autorouter-audit
    ../artifacts/rust-rewrite/transfer-build-1/autorouter-audit bootstrap --build-report ../artifacts/rust-rewrite/transfer-build-1/bootstrap-build.jsonl > ../artifacts/rust-rewrite/transfer-build-1/bootstrap-proof.json
    ../artifacts/rust-rewrite/transfer-build-1/autorouter-audit vendor > ../artifacts/rust-rewrite/transfer-build-1/vendor-audit.json
    OPENSSL_NO_VENDOR=0 cargo build --release --locked --package claude-autorouter --message-format=json > ../artifacts/rust-rewrite/transfer-build-1/ordinary-cli-build.jsonl
    ../artifacts/rust-rewrite/transfer-build-1/autorouter-audit production-features --build-report ../artifacts/rust-rewrite/transfer-build-1/ordinary-cli-build.jsonl > ../artifacts/rust-rewrite/transfer-build-1/ordinary-cli-features.json
    OPENSSL_NO_VENDOR=0 cargo build --release --locked --package xtask --bin xtask --message-format=json > ../artifacts/rust-rewrite/transfer-build-1/benchmark-helper-build.jsonl

Retain the independent bootstrap auditor before building the helper. The CLI
artifact report must pass its feature audit before benchmarking. The helper
intentionally enables development probes; its separate build does not rebuild
the CLI target. Keep the build/audit reports, and repeat the ordinary CLI audit
if that binary is rebuilt. This verifies features and current binary identity,
without approving release, platform support, model quality or performance.

First validate deterministic behavior without retaining performance values:

    ./target/release/xtask benchmark --protocol v4 --workload tiny_strings_miss --validate --output results/tiny-validation-1
    ./target/release/xtask benchmark --processing --validate --output results/processing-validation-1
    ./target/release/xtask benchmark --whole-connection --validate --output results/whole-validation-1

The v4 full/near-limit128-client family requires known physical RAM>=32GiB,
an explicit safety heuristic rather than a guarantee of available headroom.
Other selected families remain available on16GiB. Keep all declared concurrency
groups and samples; a capacity refusal remains unmeasured. Use the larger Mac
for the full near-limit matrix, never lower concurrency to make it fit.

Numerical runs are separate opt-in measurements. Record power, ordinary apps
and memory pressure, pause builds/tests, and preserve the machine's conditions.
On an adequate larger host:

    ./target/release/xtask benchmark --protocol v4 --output results/full-v4-1
    ./target/release/xtask benchmark --processing --output results/processing-1
    ./target/release/xtask benchmark --whole-connection --measure --output results/whole-measurement-1

Near-limit input at128 clients has a raw-input floor of about4GiB; a full run
moves several TiB over loopback. A run can take hours. Outputs are create-only,
with per-row evidence retained. Never overwrite/restart into an old directory.
If interrupted, preserve it as incomplete; resume investigation in a new
directory. --workload ID can collect one declared family at all concurrency
groups and five complete alternating rounds. Each filtered report names its
excluded coverage; it cannot stand in for the whole protocol or plan.

Retain this transfer/checksums, Cargo build logs, exact binary/source hashes and
every report, including failures. Existing protocols1-3 and their failures are
unchanged; never pool versions. The processing helper excludes HTTP receive,
auth/header framing, response observation/storage and final delivery, and never
independently qualifies the whole shipping gateway's0.5 processing target.
The separate whole-connection helper includes ordinary gateway/client HTTP1
codecs and requires exact loopback/held-writer/parsed-count controls before
timings. Its normal-use results remain exploratory; controlled hardware and
shipping source/binary qualification remain required. Read its frozen protocol,
review and policy-branch companion before running it.
FD warnings or unavailable measurements stay unverified. A single postload
snapshot is not a long resource soak or an allocation/retained-state result.

Real evaluator and completed-task trials are separate. Read the recorded
protocol and call --help first. No command above calls Claude, Jev or Ollama;
no API key is needed. Do not infer current-provider/model quality from mocks.
"#;

fn write(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(path)
        .map_err(|_| "Refusing to overwrite transfer evidence")?;
    file.write_all(bytes)
        .map_err(|_| "Cannot write transfer evidence".into())
}
fn reference(root: &Path) -> Result<(BTreeMap<String, Entry>, Value), String> {
    let directory = root.join("artifacts/rust-rewrite/reference");
    crate::reference::freeze(root, &directory)?;
    let bytes = crate::process::read_bounded(&root.join("rust/parity/baseline.json"), 1024 * 1024)?;
    let manifest: Value =
        serde_json::from_slice(&bytes).map_err(|_| "Invalid transfer reference manifest")?;
    let mut files = BTreeMap::new();
    for entry in manifest["files"]
        .as_array()
        .ok_or("Missing transfer reference files")?
    {
        let relative = entry["path"]
            .as_str()
            .ok_or("Invalid transfer reference path")?;
        if relative.is_empty()
            || !Path::new(relative)
                .components()
                .all(|part| matches!(part, Component::Normal(_)))
        {
            return Err("Unsafe transfer reference path".into());
        }
        let source = regular(&directory, relative)?;
        if entry["bytes"].as_u64() != Some(source.len() as u64) || entry["sha256"] != sha(&source) {
            return Err("Transfer reference changed after verification".into());
        }
        if files
            .insert(
                relative.to_owned(),
                Entry {
                    bytes: source,
                    mode: 0o644,
                },
            )
            .is_some()
        {
            return Err("Duplicate transfer reference path".into());
        }
    }
    files.insert(
        "frozen-reference-manifest.json".into(),
        Entry { bytes, mode: 0o644 },
    );
    // Keep the project's license with the independently transferable source.
    if !files.contains_key("LICENSE") {
        files.insert(
            "LICENSE".into(),
            Entry {
                bytes: regular(root, "LICENSE")?,
                mode: 0o644,
            },
        );
    }
    Ok((files, manifest))
}

pub(super) fn create(root: &Path, destination: &Path) -> Result<Value, String> {
    if fs::symlink_metadata(destination).is_ok() {
        return Err("Refusing to overwrite an existing transfer directory".into());
    }
    let source = collect(root)?;
    let source_expanded = expanded_size(&source, SOURCE)?;
    let (reference, baseline) = reference(root)?;
    let reference_policy = SOURCE.remaining(
        SOURCE
            .expanded
            .checked_sub(source_expanded)
            .ok_or("Transfer source exhausted archive budget")?,
    );
    let reference_expanded = expanded_size(&reference, reference_policy)?;
    let mut builder = fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder
        .create(destination)
        .map_err(|_| "Cannot create private transfer directory")?;
    let source_report = write_archive(
        "autorouter-benchmark",
        &source,
        &destination.join("native-source.tar.gz"),
        SOURCE,
    )?;
    let reference_report = write_archive(
        REFERENCE_ROOT,
        &reference,
        &destination.join("frozen-node-reference.tar.gz"),
        reference_policy,
    )?;
    let manifest = json!({"schema_version":1,"kind":"matched_native_benchmark_transfer","baseline_commit":baseline["baseline_commit"],"baseline_version":baseline["baseline_version"],"reference_files":baseline["files"].as_array().unwrap().len(),
        "archives":{"native-source.tar.gz":{"sha256":source_report["sha256"],"expanded_tar_bytes":source_expanded},"frozen-node-reference.tar.gz":{"sha256":reference_report["sha256"],"expanded_tar_bytes":reference_expanded}},
        "aggregate_expanded_tar_bytes":source_expanded+reference_expanded,"aggregate_expanded_limit":SOURCE.expanded,"node_runtime_required":"22.14.0 for temporary reference tests only","native_build_required":true,"private_material_included":false,"binaries_included":false,"dependencies_bundled":false,"provider_calls":0,"release_approved":false,"performance_accepted":false});
    let bytes =
        serde_json::to_vec_pretty(&manifest).map_err(|_| "Cannot encode transfer manifest")?;
    write(&destination.join("transfer-manifest.json"), &bytes)?;
    write(&destination.join("RESUME.md"), README.as_bytes())?;
    let sums = format!(
        "{}  native-source.tar.gz\n{}  frozen-node-reference.tar.gz\n{}  transfer-manifest.json\n{}  RESUME.md\n",
        source_report["sha256"].as_str().unwrap(),
        reference_report["sha256"].as_str().unwrap(),
        sha(&bytes),
        sha(README.as_bytes())
    );
    write(&destination.join("SHA256SUMS"), sums.as_bytes())?;
    Ok(manifest)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::package::archive::decode_root;
    use crate::tool_process::Scratch;
    #[test]
    fn transfer_has_only_verified_sources_a_shared_budget_and_create_only_evidence() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .canonicalize()
            .unwrap();
        let scratch = Scratch::new("benchmark-transfer").unwrap();
        let directory = scratch.0.join("transfer");
        let report = create(&root, &directory).unwrap();
        assert_eq!(report["reference_files"], 111);
        assert_eq!(report["provider_calls"], 0);
        let native_bytes = fs::read(directory.join("native-source.tar.gz")).unwrap();
        let native = decode_root(&native_bytes, "autorouter-benchmark", SOURCE).unwrap();
        let reference_bytes = fs::read(directory.join("frozen-node-reference.tar.gz")).unwrap();
        let reference = decode_root(
            &reference_bytes,
            REFERENCE_ROOT,
            SOURCE.remaining(SOURCE.expanded - native.expanded_bytes),
        )
        .unwrap();
        assert_eq!(
            native.expanded_bytes + reference.expanded_bytes,
            report["aggregate_expanded_tar_bytes"].as_u64().unwrap() as usize
        );
        assert!(native.expanded_bytes + reference.expanded_bytes <= SOURCE.expanded);
        let supplied = &reference.files;
        let manifest: Value =
            serde_json::from_slice(&supplied["frozen-reference-manifest.json"].bytes).unwrap();
        for entry in manifest["files"].as_array().unwrap() {
            let path = entry["path"].as_str().unwrap();
            assert_eq!(sha(&supplied[path].bytes), entry["sha256"]);
        }
        assert_eq!(supplied.len(), 113);
        assert_eq!(
            supplied["LICENSE"].bytes,
            fs::read(root.join("LICENSE")).unwrap()
        );
        assert!(
            supplied
                .keys()
                .all(|path| !path.starts_with(".git/") && !path.ends_with(".env"))
        );
        assert!(
            native
                .files
                .contains_key("rust/parity/local-benchmark-v4.json")
        );
        assert!(
            native
                .files
                .contains_key("rust/parity/processing-reference.mjs")
        );
        assert!(
            !native
                .files
                .contains_key("artifacts/rust-rewrite/reference/src/router.mjs")
        );
        assert!(create(&root, &directory).is_err());
        assert_eq!(
            fs::read(directory.join("native-source.tar.gz")).unwrap(),
            native_bytes
        );
        let checksum = fs::read_to_string(directory.join("SHA256SUMS")).unwrap();
        assert!(checksum.contains(&sha(&reference_bytes)));
        assert!(
            !fs::read_to_string(directory.join("RESUME.md"))
                .unwrap()
                .contains("fabio.rapposelli")
        );
    }
}
