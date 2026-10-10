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

A successful symbol gate establishes only a necessary GLIBC requirement bound.
These containers share the hosted runner's kernel; they cannot qualify an older
kernel. Transitive libraries, DNS/NSS, corporate trust stores, CPU deployment
coverage, WSL and every supported distribution still require their own evidence.
Static musl is a separate DNS/TLS and runtime qualification path. Pinning build
inputs also does not prove byte-for-byte reproducibility until independently
repeated builds are compared.

The failed aggregate 32 MiB package limit remains a separate blocker. A GNU
portability job does not waive that limit or qualify the missing architectures.
