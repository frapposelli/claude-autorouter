# Native distribution feasibility

This directory defines an experimental archive format. The shipping JavaScript npm package is unchanged. Candidate archives are private, use the existing version, and must not be published. Platform and performance gates remain pending.

The POSIX dispatcher resolves npm's symbolic links, selects a bundled executable and uses `exec` with the original arguments. It performs no downloads and has no Node fallback. Package verification rejects links, traversal, duplicate files, incorrect binary architecture, checksum drift and non-executable launchers. Both compressed and expanded archives retain the existing 32 MiB bounds.

The four historical CI archives for head `5cd7118` each passed an offline
installed smoke test with 27 CLI tests. Combining their executables was rejected:
the archive needs at least 39,386,112 expanded bytes, above the unchanged
33,554,432-byte limit. The [retained size evidence](../parity/measurements/package-ci-5cd7118-summary.json)
records each archive hash and its build-manifest source identity. These artifacts
predate the OpenSSL TLS change. Current binary sizes across all four targets, the expanded 60-test
installed suite on other targets, additional architectures and oldest-runtime
qualification remain pending; this result does not establish a shippable
aggregate package.

The later macOS ARM64 host archive passed all 60 installed CLI tests and the
offline upgrade/rollback rehearsal. Its [retained summary](../parity/measurements/native-host-4-summary.json)
records the unchanged archive hash, 5,954,022 compressed bytes and 14,446,592
expanded tar bytes. This local feasibility build includes vendored OpenSSL;
it does not establish release provenance or multi-target package feasibility.
Completed lifecycle-test failures retain bounded synthetic output in private
diagnostic files. Timeout and output-limit failures discard partial output.

The [next host archive](../parity/measurements/native-host-5-summary.json) passed
66 installed CLI tests and the same offline upgrade/rollback rehearsal after
TLS-version and hidden-input cancellation changes. It contains 5,954,732
compressed bytes and 14,449,152 expanded tar bytes. The original Linux
cancellation failure still requires a fresh Linux CI result; passing macOS
tests does not resolve that platform evidence gap.

The [corrected host archive](../parity/measurements/native-host-6-summary.json)
replaces output-draining terminal restoration with separate input discard and
immediate attribute restoration. Its stopped-output regression, 66 installed
CLI tests and offline upgrade/rollback rehearsal pass. The archive is 5,955,906
compressed bytes and 14,449,664 expanded tar bytes. Earlier archive bytes and
their test reports remain retained; real Ctrl-Z lifecycle and existing prompt
output behavior still need separate qualification.

Inspection of those same historical Linux executables found non-weak GLIBC
version requirements through **2.34 on x86_64** and **2.39 on ARM64**. Their
complete dependency/version tables match an independent LLVM `readelf` inspection;
the [hash-bound evidence](../parity/measurements/elf-ci-5cd7118-summary.json)
retains weak requirements separately. These builds therefore do not establish
compatibility with the recorded Node 22 GLIBC 2.28 baseline. A numeric version
requirement is only a lower bound: transitive libraries, loader behavior and
execution on the oldest claimed runtime still need qualification.

The separate [GNU portability workflow](gnu-portability.md) now pins older
builder images and checks a GLIBC 2.28 symbol bound. Its first run stopped at
Git provenance recording; the next exposed an ARM assembly flag override and
missing Perl core modules. Scoped compiler wrappers and a pinned build-only
Perl bootstrap are awaiting CI. Failed attempts, pins and remaining limits are
recorded there.

The initial macOS ARM64/x86_64 and Linux ARM64/x86_64 candidates do not cover every platform where the existing unrestricted JavaScript package can run. Node 22 additionally lists Linux armv7, ppc64le and s390x, plus experimental architectures. These remain audit items in [platforms.json](platforms.json), alongside Claude executable availability. See the [Node 22 platform table](https://raw.githubusercontent.com/nodejs/node/v22.x/BUILDING.md).

Rust documents macOS 11.0 ARM64 and 10.12 x86_64 minima; its deployment target can raise those requirements. The produced binary and every dependency still need inspection and execution on the oldest claimed system. See [Apple target requirements](https://doc.rust-lang.org/rustc/platform-support/apple-darwin.html). GNU target defaults and musl target availability likewise do not establish the minimum libc/kernel of a linked product; the [Rust target table](https://doc.rust-lang.org/rustc/platform-support.html) is an input to qualification, not product support evidence.

npm's `bin` mapping provides Unix symbolic links. The isolated smoke check installs the exact archive offline with `--ignore-scripts`, then runs the symlink from an unrelated directory with Node absent from PATH. The dispatcher resolves standard utilities from `/usr/bin` or `/bin` without altering the child's PATH. The same native CLI integration suite then exercises the installed executable, including launcher signals, credentials, settings, configuration and cleanup. See [npm executable mappings](https://docs.npmjs.com/cli/v11/configuring-npm/package-json/#bin).

From the Rust workspace, assemble and verify an explicitly supplied host artifact:

```sh
cargo build --release --locked --package claude-autorouter
cargo xtask package --binary aarch64-apple-darwin=rust/target/release/claude-autorouter --output artifacts/rust-rewrite/package-NEW --smoke
cargo xtask package-verify artifacts/rust-rewrite/package-NEW/claude-autorouter-0.5.2.tgz
cargo xtask package-smoke artifacts/rust-rewrite/package-NEW/claude-autorouter-0.5.2.tgz
cargo xtask package-inspect x86_64-unknown-linux-gnu PATH_TO_LINUX_BINARY
```

Choose the actual target of the supplied executable; filenames do not override architecture inspection. Paths resolve against the repository root and assembly refuses to overwrite an existing destination. Additional `--binary TARGET=PATH` arguments include other independently built artifacts. Smoke checks require the host artifact, npm for installation, and the Rust toolchain for the shared integration suite; the installed CLI itself needs no Node runtime. System certificate-store and loopback access are needed for launcher checks.

Local package assembly records binary and Cargo lock hashes, source revision/dirty state, license material and target inspection. These are artifact-integrity facts; a dirty-tree feasibility build is not release provenance. A complete release still requires every supported target, exact installed launcher lifecycle, license review, an immutable archive tested once, registry integrity verification, rollout and rollback qualification.

A source-only transfer bundle is also available:

```sh
cargo xtask benchmark-bundle --output artifacts/rust-rewrite/benchmark-source-NEW.tar.gz
```

It includes the native workspace, lockfile, synthetic fixtures, public hardware
notes, vendored parser and OpenSSL wrapper source/licenses, and a per-file checksum manifest. It
excludes configuration, credentials, captured traffic, logs, build artifacts,
and JavaScript runtime sources. The destination still needs the pinned Rust
toolchain, a C compiler, Make, Perl and locked Cargo dependencies; this is not a standalone executable
or an offline dependency vendor. Building it and requesting tool help does
not invoke an evaluator. Actual evaluator and Claude-startup commands retain
their explicit opt-in requirements.
