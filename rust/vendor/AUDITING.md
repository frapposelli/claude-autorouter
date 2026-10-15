# Native dependency and release-feature audits

Run the source auditor before building the inspected Hyper, hyper-util, Nix and
OpenSSL sources. The `autorouter-audit` contributor binary belongs to the existing
xtask crate and builds with `--no-default-features`; its graph contains no
AutoRouter runtime/core, Hyper, hyper-util, OpenSSL or Tokio dependency. The
ordinary `cargo xtask` keeps its default `runtime-tools` feature and existing
commands. The auditor rejects invocation with that feature enabled. Its Nix
dependency is the unchanged registry package without the terminal feature;
the product's [patched terminal supplier](NIX-TERMINAL.md) is a separately pinned
Git package. Actual Cargo artifact checks reject source substitution or feature
unification. Fetch locked dependencies before resolving and auditing either
supplier; the source audit performs no downloads itself.

From `rust/`, choose fresh retained report paths:

```sh
mkdir -p ../artifacts/rust-rewrite
cargo build --package xtask --no-default-features --bin autorouter-audit --locked --offline --message-format=json > ../artifacts/rust-rewrite/auditor-bootstrap-build.jsonl
cp target/debug/autorouter-audit ../artifacts/rust-rewrite/autorouter-audit-bootstrap
../artifacts/rust-rewrite/autorouter-audit-bootstrap bootstrap --build-report ../artifacts/rust-rewrite/auditor-bootstrap-build.jsonl
../artifacts/rust-rewrite/autorouter-audit-bootstrap vendor
../artifacts/rust-rewrite/autorouter-audit-bootstrap production-features --self-test
```

Fetch locked dependencies first when needed. Preserve the auditor copy before
building default workspace targets: Cargo feature unification can subsequently
replace `target/debug/autorouter-audit` with a runtime-enabled build. The
bootstrap check examines actual Cargo artifact records, requires one successful
auditor with no features, rejects inspected/runtime package artifacts and binds
the compiled binary/report hashes. This supports build isolation; it is not
production release provenance.

The source audit independently verifies the pinned provenance documents, every
retained vendor file including licenses, patch hashes, reviewed changed-file
scope and Cargo feature dependencies. It reconstructs each upstream tree with
Git in a private temporary directory, verifies all upstream hashes, reapplies the
patch and verifies the exact patched tree again. It rejects symlinks,
nonregular members, inventory drift, default/full experimental features and
changed dependencies. Reads, traversal depth, total bytes, member counts and
subprocess duration/output are bounded. There are no downloads or builds of the
inspected trees during the audit.

The root audit uses the already-locked `rustls-pki-types` PEM decoder to verify
all 149 ordered DER fingerprints, exact PEM/license hashes and pinned Node
22.14.0 provenance. That provenance digest binds the original certificate
subject labels, so changing a subject, order, count or source declaration is
rejected without depending on OpenSSL/Node name-display formatting. Updating a
vendor or root set requires reviewing its original sources, licenses and exact
provenance pins in the native auditor together.

After building the ordinary release executable with Cargo JSON messages:

```sh
../artifacts/rust-rewrite/autorouter-audit-bootstrap production-features --build-report ../artifacts/rust-rewrite/production-build.jsonl
```

This command requires exactly one successful ordinary release CLI, the exact
vendored Hyper/hyper-util identities, one exact workspace runtime artifact and
the expected release profiles. It rejects experimental request lease, raw pool,
body handoff and runtime `development-probes` features independently. It checks
the actual regular executable and retains its SHA-256. The runtime package URL,
manifest path, version and features are verified from the actual artifact
messages, including explicit-name Cargo package URL forms.

The original Node and initial native audits agree on the retained production
report and all 37 original accepted/rejected controls. The current native audit
additionally requires the runtime artifact and rejects development probes; its
self-test has one accepted and 46 rejected controls. These additional gates are
stricter than the historical Node inspector. Provenance pins and bounded,
no-follow reads also remain enforced. The old `.mjs` auditors are temporary
comparison oracles; native CI uses the independent bootstrap path.

Supported contributor feature combinations are the default full xtask and the
bootstrap auditor without default features. CI checks both:

```sh
cargo clippy --package xtask --no-default-features --all-targets --locked --offline -- -D warnings
cargo test --package xtask --no-default-features --bin autorouter-audit --locked --offline
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
```

The ordinary launcher/configuration benchmark integration targets require
`runtime-tools` and are skipped by the bootstrap feature combination. Both
auditor interfaces can be used after compilation through
`cargo xtask verify-vendor` and `cargo xtask verify-production-features`; the
separate no-default build is necessary for inspection before executing the
vendored dependency graph. Audit executables stay out of product archives.

Cargo documents [optional dependencies and features](https://doc.rust-lang.org/cargo/reference/features.html#optional-dependencies)
and [target required-features](https://doc.rust-lang.org/cargo/reference/cargo-targets.html#the-required-features-field).
