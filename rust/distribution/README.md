# Native distribution feasibility

This directory defines an experimental archive format. The shipping JavaScript npm package is unchanged. Candidate archives are private, use the existing version, and must not be published. Platform and performance gates remain pending.

The POSIX dispatcher resolves npm's symbolic links, selects a bundled executable and uses `exec` with the original arguments. It performs no downloads and has no Node fallback. Package verification rejects links, traversal, duplicate files, incorrect binary architecture, checksum drift and non-executable launchers. Native npm and direct archives use separate limits: 32 MiB compressed, 64 MiB expanded, 32 MiB per file and 256 tar entries. Historical JavaScript archives and source bundles retain 32 MiB expansion. The complete supported target matrix must fit these bounds; changing a cap does not qualify a platform or authorize publication.

The four historical CI archives for head `5cd7118` each passed an offline
installed smoke test with 27 CLI tests. Combining their executables was rejected:
the archive needs at least 39,386,112 expanded bytes, above the unchanged
33,554,432-byte limit. The [retained size evidence](../parity/measurements/package-ci-5cd7118-summary.json)
records each archive hash and its build-manifest source identity. These artifacts
predate the OpenSSL TLS change. They remain historical evidence; this result
does not establish a shippable aggregate package.

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
cancellation failure was still unresolved at that checkpoint; the later
cross-platform result below supplies the missing evidence.

The [corrected host archive](../parity/measurements/native-host-6-summary.json)
replaces output-draining terminal restoration with separate input discard and
immediate attribute restoration. Its stopped-output regression, 66 installed
CLI tests and offline upgrade/rollback rehearsal pass. The archive is 5,955,906
compressed bytes and 14,449,664 expanded tar bytes. Earlier archive bytes and
their test reports remain retained; real Ctrl-Z lifecycle and existing prompt
output behavior still need separate qualification.

At `8e94a9b`, [all ten CI jobs passed](../parity/measurements/ci-8e94-qualification-summary.json).
Each of six native builds passed 463 source tests and 66 installed CLI tests;
the GNU builds also passed in pinned GLIBC 2.28 userlands. The exact four-target
selection contains 57,183,424 executable bytes and needs at least 58,833,408
expanded bytes with package materials, so the unchanged 32 MiB cap rejects it.
No aggregate archive was produced by that attempt. The 115 supported TLS cases
pass, while all 11 separate policy characterizations remain unqualified in
those production binaries. Later test-only transport experiments do not change
that archive qualification.

The [fourth local checkpoint](../parity/measurements/native-host-7-summary.json)
passes 73 installed CLI tests and upgrade/rollback. Its runtime executable is
byte-identical to host 6; new tests cover local diagnosis, default Ollama launch
and invalid Stop-hook cap admission. The new archive retains updated license
and source metadata.

An [isolated cap experiment](expanded-cap-proposal.md) assembled those four CI
binaries into a 23,943,704-byte compressed, 58,821,120-byte expanded archive and
passed the matching host installed tests. That retained experiment supported
the separately reviewed native bounds now implemented in the Rust readers.
Historical reports remain unchanged. Full-matrix fit and final-artifact
qualification are still required.

The [main-policy replay](../parity/measurements/native-cap-policy-main-summary.json)
packages the same four CI binaries with the current reader policy: 23,943,910
compressed bytes and 58,835,456 expanded bytes. Its exact archive passed all
73 installed host tests; the retained old verifier rejects it at 32 MiB expansion.
The new streaming reader also passed exact-bound and malformed-input tests.
These are private feasibility and local resource observations, not qualification
of later runtime changes, foreign execution, the complete platform matrix or a
release artifact.

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
Perl bootstrap now pass both jobs, including symbol inspection and installed
lifecycle checks. Failed attempts, pins and remaining limits are recorded there.

The initial macOS ARM64/x86_64 and Linux ARM64/x86_64 candidates do not cover every platform where the existing unrestricted JavaScript package can run. Node 22 additionally lists Linux armv7, ppc64le and s390x, plus experimental architectures. These remain audit items in [platforms.json](platforms.json), alongside Claude executable availability. See the [Node 22 platform table](https://raw.githubusercontent.com/nodejs/node/v22.x/BUILDING.md).

The package reader also recognizes pending `i686-unknown-linux-gnu` artifacts
as little-endian ELF32 with machine EM_386. A 32-bit Linux Rust process maps to
that target; Linux `i386`, `i486`, `i586` and `i686` machine names select it in the
dispatcher. These mappings permit synthetic artifact tests and do not qualify
older CPUs or runtimes. Linux x86 remains unresolved and the target remains a
non-candidate pending full qualification. On an x64 Linux kernel, tests of genuine
i686 executables use an owned `setarch i686` child so real `uname` and installed
dispatch agree; nested package tests also need `CARGO_BUILD_TARGET=i686-unknown-linux-gnu`.

Rust documents macOS 11.0 ARM64 and 10.12 x86_64 minima; its deployment target can raise those requirements. The produced binary and every dependency still need inspection and execution on the oldest claimed system. See [Apple target requirements](https://doc.rust-lang.org/rustc/platform-support/apple-darwin.html). GNU target defaults and musl target availability likewise do not establish the minimum libc/kernel of a linked product; the [Rust target table](https://doc.rust-lang.org/rustc/platform-support.html) is an input to qualification, not product support evidence.

ARMv7 `gnueabihf` header inspection requires EABI5 and explicit hard-float flags,
and rejects soft-float or conflicting declarations, following the
[Arm ELF header specification](https://github.com/ARM-software/abi-aa/blob/main/aaelf32/aaelf32.rst#52-elf-header).
These checks do not qualify ARMv7 CPU instructions, the operating system or an
installed executable. The target remains pending and is not a release candidate.

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

An exact verified npm feasibility archive can also supply a private direct
archive for one of its bundled targets:

```sh
cargo xtask package --direct-from artifacts/rust-rewrite/package-NEW/claude-autorouter-0.5.2.tgz --target aarch64-apple-darwin --output artifacts/rust-rewrite/direct-NEW --smoke
```

Choose the actual bundled target and a fresh output directory with an existing
parent. The converter checks the npm checksum sidecar, copies the same binary,
documentation and licenses, and records the parent archive's exact hash. The
direct candidate has schema 1, `kind: "native_direct_feasibility"`,
`private: true` and `release_approved: false`. It retains the original source
identity and pending qualification; production release checks reject it.

`--smoke` extracts that exact bounded archive into a private temporary directory,
verifies installed bytes and permissions, runs help/version without Node on
`PATH`, and runs the same seven installed launcher/configuration suites used by
npm smoke. No npm installation, provider request or real user configuration is
involved. Installation paths include spaces, quotes and dollar signs; execution
uses an unrelated working directory. An incompatible CPU/OS is rejected before
execution. Cargo and contributor test dependencies are needed by this validation
tool, not by the installed native executable. Temporary installations are
removed on success and failure. Assembled archives stay unchanged for diagnosis
if a completed smoke fails.

Repeat the test of an unchanged direct archive with
`cargo xtask release-pack smoke-direct ../artifacts/rust-rewrite/direct-NEW/claude-autorouter-0.5.2-aarch64-apple-darwin.tar.gz`.
This command can test private feasibility or qualified production candidates;
its report never supplies final release authorization. Oldest OS/libc, CPU
fleet, corporate CA environments, real Keychain entries and live providers need
their own evidence. The [platform audit](platform-audit.md) records why the
larger baseline matrix remains unresolved.

A source-only transfer bundle is also available:

```sh
cargo xtask benchmark-bundle artifacts/rust-rewrite/benchmark-source-NEW.cpio.gz
```

The default source container is gzip binary big-endian CPIO with the original
schema2 JSON inventory; GNUtar alone cannot read it. Use `benchmark-bundle
--inspect-source binary-cpio ARCHIVE` or `--extract-source binary-cpio ARCHIVE
NEW_ABSOLUTE_DIRECTORY` for bounded verification/restoration.
`--source-format ustar NEW_ARCHIVE.tar.gz` explicitly selects the legacy source
tar format and its unchanged limit. See the [source extraction guide](../docs/local-model-comparison.md#complete-native-source-without-a-ready-executable).

It includes the native workspace, lockfile, synthetic fixtures, public hardware
notes, vendored parser and OpenSSL wrapper source/licenses, and a per-file checksum manifest. It
excludes configuration, credentials, captured traffic, logs, build artifacts,
and frozen JavaScript product sources. A temporary Node comparison adapter
is included because native live-reference tooling embeds it; using that path
still requires a separately verified frozen checkout and Node runtime. The destination still needs the pinned Rust
toolchain, a C compiler, Make, Perl and locked Cargo dependencies; this is not a standalone executable
or an offline dependency vendor. Building it and requesting tool help does
not invoke an evaluator. Actual evaluator and Claude-startup commands retain
their explicit opt-in requirements.

Source files retain a 1 MiB limit except the seven synthetic corpora explicitly
named in the bundle allowlist (4 MiB each).
The source archive retains its 32 MiB expanded bound. Package tests check that
all literal `include_str!`/`include_bytes!` inputs are present in the actual archive.
