//! Explicit temporary-reference handoff, separate from default Rust-only bundle.
#[cfg(test)]
use super::{SOURCE, sha};
use serde_json::Value;
#[cfg(test)]
use std::fs;
use std::path::Path;

pub(super) const README: &str = r#"# Matched Rust rewrite benchmark transfer

This directory contains public source and synthetic fixtures plus the exact
temporary Node compatibility reference. It contains no built executable, Cargo
cache, credentials, user configuration, real transcripts or private reports.
The reference is only for migration comparisons and is not a product sidecar.
This transfer does not establish release approval or performance acceptance.

Choose exactly one extraction sequence below in a dedicated Bash/Zsh shell,
starting in the transfer directory. The new private directory must not exist;
fail-fast commands stop before extraction if creation or checksums fail.
For BSD/libarchive tar (including the installed macOS tar):

    set -eu
    shasum -a 256 -c SHA256SUMS
    mkdir -m 700 native-restore
    tar -xzf native-source.cpio.gz -C native-restore
    cd native-restore/autorouter-benchmark/rust

The licensed reference delta is unusable alone: its two shared synthetic fixture
files must first be restored from the complete native archive. LICENSE remains
physically present in both carriers. The version3 stored carrier expansion and full
logical file payload each retain a separate32MiB cap; legacy full-tar expansion
is reported separately and may exceed that cap.

The native carrier is gzip newc CPIO. On macOS, the installed libarchive-based
tar reads it; the command above requires that BSD/libarchive implementation.
On Linux use an actual BSD/libarchive tar as above, or choose this GNUcpio
alternative from the transfer directory (GNUtar alone is insufficient).
Do not run both alternatives or reuse an existing extraction directory:

    set -euo pipefail
    shasum -a 256 -c SHA256SUMS
    mkdir -m 700 native-restore
    cd native-restore
    gzip -dc ../native-source.cpio.gz | cpio -idm --no-absolute-filenames --no-preserve-owner
    cd autorouter-benchmark/rust

Verify authenticated checksums before standard-tool trusted-source extraction;
these commands are not arbitrary hostile-archive security validation. The full
native source is present; no reference, Node or Git is needed to build the helper.
Actual Linux/oldest-platform bootstrap qualification remains separately required.

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
    mkdir -p ../artifacts/rust-rewrite/transfer-build-1
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

After the helper build, restore the complete111-file frozen reference plus its
original manifest and license before any reference operation. The receiver
verifies both archives, every fixture/hash/mode, both budgets and checksums
before creating a private destination. Existing destinations are never replaced.
This command also reads historical schema1 full-reference and schema2 tar-delta transfers:

    ./target/release/xtask benchmark-bundle --restore-reference TRANSFER_ABSOLUTE_PATH artifacts/rust-rewrite/reference

Use the absolute path of the directory containing both carriers and their outer
checksums; the new reference path is relative to the extracted native root.
A failed/interrupted restore leaves an incomplete private directory; preserve it
and select a new destination. No Git, Node, evaluator or provider is called.

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

Portable paired local-model trials are separately opt-in and need no Git or
private wrapper. Use an already-installed local decision model and pinned
Node22.14 executable. Pause builds/tests and other Ollama requests; ordinary
apps may remain open. Read rust/docs/local-model-comparison.md and the immutable
local-model-orchestration-v1 protocol, then explicitly prepare without inference:

    ./target/release/xtask compare-local-evaluators prepare --model INSTALLED_TAG --node /absolute/path/to/node22.14 --warm-timeout-ms 1500 --conditions normal-use-exclusive-ollama-builds-paused --output results/model-finite-declaration

Choose1500, or0 for a new separate zero-warm-deadline declaration.
Prepare freezes exact source/reference/
fixture/model/runtime/binary identities and downloads/loads/unloads nothing.
Run explicitly performs990 local synthetic model calls if every cold call
succeeds, preserving five alternating Node/native pairs and all original gates:

    ./target/release/xtask compare-local-evaluators run --declaration results/model-finite-declaration --conditions normal-use-exclusive-ollama-builds-paused --output results/model-finite-run-1

Every original report, failure and finite-deadline availability mismatch stays
visible. Conditions are separately declared/observed; these model trials do not
qualify controlled gateway performance, completed Claude tasks or release.
"#;

pub(super) fn create(root: &Path, destination: &Path) -> Result<Value, String> {
    super::pair::create(root, destination)
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
        let native_bytes = fs::read(directory.join("native-source.cpio.gz")).unwrap();
        let native = crate::package::archive::cpio::decode_root(
            &native_bytes,
            "autorouter-benchmark",
            SOURCE,
            |path| {
                if super::super::LARGE_SYNTHETIC_CORPORA.contains(&path) {
                    4 * 1024 * 1024
                } else {
                    1024 * 1024
                }
            },
        )
        .unwrap();
        let reference_bytes =
            fs::read(directory.join("frozen-node-reference-delta.tar.gz")).unwrap();
        let reference = decode_root(
            &reference_bytes,
            "autorouter-node-reference-delta",
            SOURCE.remaining(SOURCE.expanded - native.expanded_bytes),
        )
        .unwrap();
        assert_eq!(
            native.expanded_bytes + reference.expanded_bytes,
            report["aggregate_expanded_carrier_bytes"].as_u64().unwrap() as usize
        );
        assert!(native.expanded_bytes + reference.expanded_bytes <= SOURCE.expanded);
        assert_eq!(reference.files.len(), 111);
        assert_eq!(report["shared_inputs"].as_array().unwrap().len(), 2);
        let restored = scratch.0.join("restored");
        super::super::pair::restore(&root, &directory, &restored).unwrap();
        let baseline: Value = serde_json::from_slice(
            &fs::read(restored.join("frozen-reference-manifest.json")).unwrap(),
        )
        .unwrap();
        let supplied: std::collections::BTreeMap<_, _> = baseline["files"]
            .as_array()
            .unwrap()
            .iter()
            .map(|entry| entry["path"].as_str().unwrap())
            .chain(["LICENSE", "frozen-reference-manifest.json"])
            .map(|path| {
                (
                    path.to_owned(),
                    super::super::Entry {
                        bytes: fs::read(restored.join(path)).unwrap(),
                        mode: 0o644,
                    },
                )
            })
            .collect();
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
                .contains_key("rust/parity/local-benchmark-v5.json")
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
            fs::read(directory.join("native-source.cpio.gz")).unwrap(),
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
