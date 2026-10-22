# GNU portability checks

The [separate workflow](../../.github/workflows/rust-portability.yml) builds and
runs native x86_64 and ARM64 candidates in pinned GLIBC 2.28 userlands. Both
jobs passed at `8e94a9b`: 463 source tests, 66 installed CLI tests, the shared
differential suites and the GLIBC 2.28 symbol gate. The
[retained CI summary](../parity/measurements/ci-8e94-qualification-summary.json)
binds exact source trees, binaries, archives and logs. This establishes the
recorded userland checks; oldest-kernel and complete platform qualification
remain pending. The existing CI jobs remain required.

Earlier attempts remain retained. The first run installed both pinned
toolchains but stopped at Git provenance recording because the container and
runner checkout had different owners. The workflow now trusts only the exact
runner checkout path and fails immediately if source-timestamp lookup fails.
The next run reached compilation and exposed two build prerequisites: global
ARM64 `CFLAGS` overrode the hashing crate's specialized assembly flags, and the
minimal Perl installation lacked `IPC::Cmd`. Both failed runs are retained in
the pin record. The current corrections use scoped compiler wrappers and a
checksum-pinned, build-only Perl installation. Those prerequisites now pass.
A subsequent source run exposed terminal and launcher test failures; the
`8e94a9b` correction passes on both architectures.

The historical Ubuntu 24 builds have required GLIBC symbol versions through
2.34 on x86_64 and 2.39 on ARM64. Those values come from the actual binaries,
independently checked against LLVM `readelf`, rather than Rust target defaults.
See the [complete retained inspection](../parity/measurements/elf-ci-5cd7118-summary.json).

The builder images are PyPA's `manylinux_2_28_x86_64` and
`manylinux_2_28_aarch64`, which use AlmaLinux 8 and GCC 14. Their purpose here is
to provide a controlled older GNU build environment; a Python-wheel compatibility
label does not certify this executable. [PyPA's image documentation](https://github.com/pypa/manylinux#manylinux_2_28-almalinux-8-based)
describes the base and architectures. The [pin record](gnu-builder-pins.json)
retains the resolved immutable image digests, registry metadata dates, both
Rust archive checksums, and the Perl source checksum. The Perl archive was
downloaded and verified locally for inspection. The images, standalone Rust
archives and Perl bootstrap have not been executed locally.

The workflow uses the existing `ubuntu-24.04` and `ubuntu-24.04-arm` native
runners, with a pinned container per architecture. GitHub documents both
[native runner labels](https://docs.github.com/en/actions/reference/runners/github-hosted-runners)
and [job containers](https://docs.github.com/en/actions/how-tos/write-workflows/choose-where-workflows-run/run-jobs-in-a-container).
It explicitly selects Bash because container steps otherwise default to `sh`.

Each job performs these steps:

1. Create compiler wrappers under `/opt/autorouter-c-compilers`, outside `PATH`.
   Each invokes the original absolute compiler path with baseline CPU flags
   before the supplied arguments: x86-64/generic tuning or ARMv8-A. This preserves
   later library-specific assembly flags; global `CFLAGS/CXXFLAGS` contain only
   `-O2`. SHA-2 retains runtime CPU detection and its software fallback. Reject
   target-specific environment overrides that could bypass these defaults.
2. Build Perl 5.42.3 from the SHA-256-pinned CPAN source under
   `/opt/autorouter-perl`, with its core modules. OpenSSL's upstream
   [Perl requirements](https://raw.githubusercontent.com/openssl/openssl/openssl-3.6.3/NOTES-PERL.md)
   explain the missing modules in minimal RPM installations. This isolated
   bootstrap changes no system RPM packages and is neither shipped nor required
   by the native executable. `OPENSSL_SRC_PERL` selects it explicitly.
3. Verify the machine architecture and `getconf GNU_LIBC_VERSION` before the
   Rust build. Download the exact Rust 1.99.0 standalone archive from
   `static.rust-lang.org`, check its committed SHA-256, and install it under
   `/opt/autorouter-rust` with the upstream installer and documentation omitted.
   The Cargo cache is isolated under `/opt/autorouter-cargo`. These paths are
   inside the disposable job container; no runner-default toolchain is changed.
4. Record the actual Git commit, lockfile and workflow hashes, immutable image,
   toolchains, C compiler, linker, installed RPM versions, kernel and declared
   compiler flags. Retain wrapper contents/hashes, effective compiler target
   options and baseline preprocessor macros. Only specific public build variables
   are recorded.
5. Fetch the entire locked dependency graph for license inventory. Check vendor
   provenance, formatting, clippy, native tests and the complete shared
   differential suite. Rust flags explicitly select x86-64 or generic ARM64;
   `target-cpu=native` is never used. Specialized assembly is allowed only where
   the dependency dispatches it at runtime. OpenSSL uses the locked vendored source.
6. Build the release executable, retain its exact dependency/version inspection
   and independent GNU `readelf` output, then enforce
   `package-inspect TARGET BINARY --max-glibc 2.28`. This gate compares every
   non-weak numeric GLIBC requirement, rejects unresolved nonnumeric GLIBC ABI
   markers, and does not accept absent version requirements as portability proof.
   Weak requirements remain visible. Host OpenSSL dependencies and embedded
   RPATH/RUNPATH are rejected.
7. Exercise the synthetic executable HTTP, TLS, configuration and startup
   comparisons, then install the exact private archive offline with lifecycle
   scripts disabled and run the installed CLI suites. Native tests and installed
   smoke remove the builder's `LD_LIBRARY_PATH` and `LD_PRELOAD` overrides.
   The CLI itself runs with Node absent from its execution PATH.
8. Upload provenance, ELF reports, parity reports and the exact archive even
   when a later check fails. No provider calls, model downloads, release tags,
   registry publication or production approval are part of this workflow.

## GNU binary provenance producer

The `autorouter-audit binary-provenance` producer binds the ordinary
release executable to observed build inputs in the two reviewed GNU jobs. The
[original GNU uploads](../parity/measurements/native-gnu-original-provenance-artifacts-summary.json)
now verify both cold builds at PR head `7d6392f` and merge checkout `1a5f229`.
This is build provenance, not full platform or release qualification. Earlier
portability passes and local private archives retain their original provenance;
they cannot supply the new producer's successful execution evidence.

The workflow uses the retained, separately built `autorouter-audit-bootstrap`
with `runtime-tools` disabled. From `rust/`, after the existing pinned toolchain,
builder-record and auditor checks, the exact order is:

```sh
set -euo pipefail
set -o noclobber
unset LD_LIBRARY_PATH PKG_CONFIG_PATH
autorouter_provenance_dir="$GITHUB_WORKSPACE/artifacts/rust-rewrite/gnu-portability"
"$autorouter_provenance_dir/autorouter-audit-bootstrap" binary-provenance begin \
  --target "$AUTOROUTER_BUILD_TARGET" \
  --builder-report "$autorouter_provenance_dir/builder.json" \
  --build-report "$autorouter_provenance_dir/production-build.jsonl" \
  --feature-report "$autorouter_provenance_dir/production-features.json" \
  --out "$autorouter_provenance_dir/production-inputs.json"
cargo build --release --offline --locked --package claude-autorouter --message-format=json \
  > "$autorouter_provenance_dir/production-build.jsonl"
"$autorouter_provenance_dir/autorouter-audit-bootstrap" production-features \
  --build-report "$autorouter_provenance_dir/production-build.jsonl" \
  > "$autorouter_provenance_dir/production-features.json"
"$autorouter_provenance_dir/autorouter-audit-bootstrap" binary-provenance finish \
  --inputs "$autorouter_provenance_dir/production-inputs.json" \
  --build-report "$autorouter_provenance_dir/production-build.jsonl" \
  --feature-report "$autorouter_provenance_dir/production-features.json" \
  --out "$autorouter_provenance_dir/production-native"
```

The existing output parent must resolve without symlinks. `begin` requires
absolute, distinct, unused paths for the Cargo report, feature report and input
record. The selected Cargo `release` directory must not exist: by default this
is `rust/target/release`, or `release` under an explicitly absolute
`CARGO_TARGET_DIR`. If an earlier check created it, review an isolated fresh
release root; the producer never deletes a cache or certifies cached artifacts.
Every `compiler-artifact` in the finished Cargo report must have `fresh: false`.

`finish` requires the exact report paths declared by `begin` and a new absolute
output directory. It retains `production-native/claude-autorouter` with mode
0755 and `production-native/native-binary-provenance.json` with mode 0600 inside
a mode 0700 directory. It verifies their exact bytes, modes and owned file and
directory identities before success. Existing or replaced paths are preserved
on refusal; cleanup removes only files it created, without recursive deletion.
The workflow retains these outputs alongside the input, Cargo, feature and
builder records. Review the original run, attempt and uploaded artifact identity
with their hashes before consuming the binary/provenance pair.

Admission checks the actual clean checkout and lockfile, reviewed Cargo
configuration, compiler and wrapper identities/bytes, flags and public workflow
context before and after the build. It recomputes the original production
feature audit, matches the unique Cargo executable, and checks its GNU target,
GLIBC requirements, dependencies and search paths. Selected build variables
and input/output bytes are bounded; unknown compiler overrides are refused
without printing their values. Metadata subprocesses use a cleared environment.
The collector runs no product, build, installer, download or provider request.
The workflow's explicit Cargo command performs the build between its phases.

These records are declarations from the reviewed checksum-pinned workflow, not
signatures. The collector cannot authenticate GitHub environment variables,
prove an arbitrary caller ran the declared command, rule out transient hidden
mutation, or independently hash the entire compiler shared-library closure.
The trusted workflow/install and original artifact identity remain the review
boundary. Local runs, other target profiles and unreviewed reusable-workflow
callers are refused. Synthetic collector controls remain separate from actual
GNU build provenance. Passing this producer preserves every platform, TLS,
performance, license, lifecycle and release approval gate below and in the
[release schema](release-schema.md).

A successful symbol gate establishes only a necessary GLIBC requirement bound.
These containers share the hosted runner's kernel; they cannot qualify an older
kernel. Transitive libraries, DNS/NSS, corporate trust stores, CPU deployment
coverage, WSL and every supported distribution still require their own evidence.
Static musl is a separate DNS/TLS and runtime qualification path. Pinning build
inputs also does not prove byte-for-byte reproducibility until independently
repeated builds are compared.

The failed aggregate 32 MiB package limit remains a separate blocker. A GNU
portability job does not waive that limit or qualify the missing architectures.
