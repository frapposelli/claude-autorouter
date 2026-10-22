# Hosted macOS build provenance

The independent `autorouter-audit binary-provenance` collector has separate
profiles for `aarch64-apple-darwin` on `macos-15` and `x86_64-apple-darwin` on
`macos-15-intel`. These are the `rust` jobs in [CI](../../.github/workflows/ci.yml).
Actual uploaded Mac provenance from these new profiles remains pending.
The [GNU profiles](gnu-portability.md#gnu-binary-provenance-producer) retain
their pinned container, standalone toolchain and builder-report requirements.

The Mac profiles observe the hosted runner's installed tools. They do not claim
that the mutable runner label or SDK is a checksum-pinned, reproducible compiler
closure. The checked-in toolchain selects Rust 1.99.0; the trusted CI setup may
install it before collection. The collector itself neither installs tools nor
builds or executes the product.

The release step runs after independent auditor tests and before workspace tests
can populate `target/release`. In one shell, with failure checking and no-clobber
redirections, it normalizes the tool/SDK environment and runs:

```sh
"$autorouter_evidence_dir/autorouter-audit-bootstrap" binary-provenance begin \
  --target "$AUTOROUTER_BUILD_TARGET" \
  --build-report "$autorouter_evidence_dir/production-build.jsonl" \
  --feature-report "$autorouter_evidence_dir/production-features.json" \
  --out "$autorouter_evidence_dir/production-inputs.json"
cargo build --release --offline --locked --package claude-autorouter \
  --message-format=json > "$autorouter_evidence_dir/production-build.jsonl"
"$autorouter_evidence_dir/autorouter-audit-bootstrap" production-features \
  --build-report "$autorouter_evidence_dir/production-build.jsonl" \
  > "$autorouter_evidence_dir/production-features.json"
"$autorouter_evidence_dir/autorouter-audit-bootstrap" binary-provenance finish \
  --inputs "$autorouter_evidence_dir/production-inputs.json" \
  --build-report "$autorouter_evidence_dir/production-build.jsonl" \
  --feature-report "$autorouter_evidence_dir/production-features.json" \
  --out "$autorouter_evidence_dir/production-native"
```

Use the workflow's complete environment setup; this excerpt is not a local
build recipe. Mac `begin` must omit `--builder-report`, while GNU must supply it.
All declared paths are absolute, separate and unused. The release tree must be
absent, and every completed Cargo compiler artifact must have `fresh: false`.
The collector never clears a cache to satisfy these checks.

The Mac input witness records:

- The clean exact Git checkout, lockfile, manifest, toolchain, Cargo config and
  workflow; the actual workflow/run/attempt and hosted runner architecture.
- Rustup and its invocation shims, the actual `rustup which` compiler/Cargo,
  compiler version/host/sysroot and compiler-driver library. Rustup metadata uses
  the admitted invocation name while the witness records its canonical file and
  invocation path separately; resolving an alias must not select installer mode.
  Missing installed toolchains are rejected before asking rustup to resolve them.
- Developer directory, selected SDK/version/settings, system tool entrypoints
  and selected clang/linker/archiver identities. C++ driver invocation names are
  preserved even when `clang++` resolves to the same file as `clang`.
- An explicit portable Rust CPU profile, macOS deployment target `11.0`, vendored
  static OpenSSL settings and selected public build variables. Unreviewed
  compiler, Cargo, loader, pkg-config and target/profile overrides are rejected.
  Effective Cargo home/ancestor config paths are checked too.

Tools are hashed with a fixed-size read buffer and a separate 512 MiB supplier
bound because Xcode clang exceeds the GNU tool bound. This does not change any
product, archive, report, cancellation or runtime limit. Inputs are observed
again before retaining the output; this detects ordinary drift, rather than
proving the absence of hidden transient mutations or every SDK/library change.

`finish` reuses the exact production-feature/supplier audit and owned-output
rules. Mach-O inspection requires the target architecture, macOS platform, a
single minimum-version declaration of `11.0.0`, system-only library names and no
`LC_RPATH`. Header inspection is not execution on macOS 11 or proof that every
transitive system dependency works there.

The upload contains inputs, Cargo/feature reports, the retained executable and
schema-1 provenance. The Mac builder-report hash is null; SDK/tool identities
are bound through the input-record hash. The provenance is a trusted workflow
declaration, not a signature or release approval. Original GitHub artifact/run
association and separate platform, TLS, performance, lifecycle and final archive
qualification remain required by the [release schema](release-schema.md).

GitHub documents the public runner/workflow variables in its
[variables reference](https://docs.github.com/en/actions/reference/workflows-and-actions/variables).
Mach-O structural checks follow Apple's SDK `mach-o/loader.h` definitions for
`mach_header_64`, load commands, platform/version commands and dylib commands.
