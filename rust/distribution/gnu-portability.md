# GNU portability checks

The [separate workflow](../../.github/workflows/rust-portability.yml) builds and
runs native x86_64 and ARM64 candidates in pinned GLIBC 2.28 userlands. It is
implemented but has not yet run; no successful artifact or compatibility result
is claimed. The existing CI jobs remain required.

The historical Ubuntu 24 builds have required GLIBC symbol versions through
2.34 on x86_64 and 2.39 on ARM64. Those values come from the actual binaries,
independently checked against LLVM `readelf`, rather than Rust target defaults.
See the [complete retained inspection](../parity/measurements/elf-ci-5cd7118-summary.json).

The builder images are PyPA's `manylinux_2_28_x86_64` and
`manylinux_2_28_aarch64`, which use AlmaLinux 8 and GCC 14. Their purpose here is
to provide a controlled older GNU build environment; a Python-wheel compatibility
label does not certify this executable. [PyPA's image documentation](https://github.com/pypa/manylinux#manylinux_2_28-almalinux-8-based)
describes the base and architectures. The [pin record](gnu-builder-pins.json)
retains the resolved immutable image digests, registry metadata dates, and both
Rust archive checksums. Only this public metadata was downloaded during local
preparation; the images and standalone Rust archives have not been executed
locally.

The workflow uses the existing `ubuntu-24.04` and `ubuntu-24.04-arm` native
runners, with a pinned container per architecture. GitHub documents both
[native runner labels](https://docs.github.com/en/actions/reference/runners/github-hosted-runners)
and [job containers](https://docs.github.com/en/actions/how-tos/write-workflows/choose-where-workflows-run/run-jobs-in-a-container).
It explicitly selects Bash because container steps otherwise default to `sh`.

Each job performs these steps:

1. Verify the machine architecture and `getconf GNU_LIBC_VERSION` before the
   build. Download the exact Rust 1.99.0 standalone archive from
   `static.rust-lang.org`, check its committed SHA-256, and install it under
   `/opt/autorouter-rust` with the upstream installer and documentation omitted.
   The Cargo cache is isolated under `/opt/autorouter-cargo`. These paths are
   inside the disposable job container; no runner-default toolchain is changed.
2. Record the actual Git commit, lockfile and workflow hashes, immutable image,
   toolchain, C compiler, linker, installed RPM versions, kernel and declared
   compiler flags. Only specific public build variables are recorded.
3. Fetch the entire locked dependency graph for license inventory. Check vendor
   provenance, formatting, clippy, native tests and the complete shared
   differential suite. CPU flags explicitly select x86-64 or ARMv8-A/generic;
   `target-cpu=native` is never used. OpenSSL must use the locked vendored source.
4. Build the release executable, retain its exact dependency/version inspection
   and independent GNU `readelf` output, then enforce
   `package-inspect TARGET BINARY --max-glibc 2.28`. This gate compares every
   non-weak numeric GLIBC requirement, rejects unresolved nonnumeric GLIBC ABI
   markers, and does not accept absent version requirements as portability proof.
   Weak requirements remain visible. Host OpenSSL dependencies and embedded
   RPATH/RUNPATH are rejected.
5. Exercise the synthetic executable HTTP, TLS, configuration and startup
   comparisons, then install the exact private archive offline with lifecycle
   scripts disabled and run the installed CLI suites. Native tests and installed
   smoke remove the builder's `LD_LIBRARY_PATH` and `LD_PRELOAD` overrides.
   The CLI itself runs with Node absent from its execution PATH.
6. Upload provenance, ELF reports, parity reports and the exact archive even
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
