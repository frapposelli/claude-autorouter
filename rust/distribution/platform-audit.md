# Native platform qualification audit

Reviewed October 13, 2026. This audit retains the unrestricted
`claude-autorouter@0.5.2` Node 22 baseline. It does not approve a release matrix,
raise archive bounds or remove targets from [platforms.json](platforms.json).

The [Node 22 platform table](https://raw.githubusercontent.com/nodejs/node/v22.x/BUILDING.md)
includes Linux x64, ARM64 and ARMv7 at tier 1, POWER8+ ppc64le and s390x at tier
2, plus experimental x86, ARMv6, musl and LoongArch environments. Its macOS
ARM64/x64 floor is 11.0. GNU tier-1/2 rows use kernel 4.18 and glibc 2.28;
experimental ports have separate requirements. Rust target availability or a
modern runner cannot establish execution at those floors.

The current [Claude Code setup guide](https://code.claude.com/docs/en/setup#system-requirements)
lists x64/ARM64 hardware, macOS 13+, Ubuntu 20.04+, Debian 10+ and Alpine 3.19+.
Its [npm installation section](https://code.claude.com/docs/en/setup#install-with-npm)
lists six native macOS/Linux variants, with GNU and musl Linux alternatives.
These narrower requirements are Claude installation evidence, not proof that
the AutoRouter baseline cannot run on other architectures: configuration,
history and the standalone gateway do not require launching Claude. Historical
Claude installations and ESM embedding consumers need their own audit. Keep
each wider baseline target pending until its actual behavior is qualified or
retained evidence establishes baseline unavailability.

Rust's [Apple target notes](https://doc.rust-lang.org/rustc/platform-support/apple-darwin.html)
allow deployment-target selection and state different compiler target minima.
Inspect the actual Mach-O load commands and dependency requirements, then run
the exact installed artifact on the oldest claimed macOS system. Raising the
AutoRouter floor to match current Claude documentation alone would change the
baseline contract. The same distinction applies to GNU versioned symbols and
the actual kernel, loader, DNS and TLS environment.

Claude documents both WSL 1 and WSL 2, with different sandbox availability. WSL
2 uses a real Linux kernel; WSL 1 translates calls. Test both terminal/signal and
network behavior explicitly rather than inferring WSL from an Ubuntu CI job.
[Microsoft's comparison](https://learn.microsoft.com/en-us/windows/wsl/compare-versions)
describes this architecture difference. Native Windows remains outside the
rewrite scope.

## Evidence to retain for each target

| Gate | Evidence and execution site |
| --- | --- |
| Oldest OS/libc/kernel | Actual exact-artifact execution on the recorded oldest OS and kernel; a container userland establishes libc behavior only. GNU CI currently supplies glibc-2.28 userland and symbol checks. |
| CPU and linking | Portable compiler flags and toolchain/build hashes, executable header inspection, actual dependencies and CPU requirements, and execution on the minimum CPU fleet. A cross-build is not an execution result. |
| TLS/DNS | Existing synthetic transport/security/CA comparisons on the installed artifact, plus explicit system/corporate/custom-CA qualification; no private roots or captured credentials in committed evidence. |
| Installed lifecycle | Offline npm install with `--ignore-scripts`, runtime execution without Node, exact direct-archive smoke, all seven installed CLI suites, PTY/signal cleanup and failed startup. |
| Migration/rollback | Exact native npm archive and frozen baseline through isolated `upgrade-rollback`; separate real Keychain identity/migration qualification on macOS. |
| WSL | WSL 1/2 runtime versions, Linux distribution/libc, terminal signals, gateway networking and installed lifecycle results; no native Windows implementation implied. |
| Full archive fit | Every required target's final immutable executable in the aggregate npm archive; independently enforce 32 MiB compressed, 64 MiB expanded, 32 MiB per file and 256 entries. |
| Trusted source build | Same clean source commit and Cargo lock for every binary; original CI workflow run/artifact identity and checksums; dependency/license review and hash-bound final reports. |

The existing release assembler preserves every declared baseline target and
requires explicit retained resolutions for the unresolved x86, ARMv6 and other
ports. Current Claude availability cannot substitute for those resolutions.
The historical four-target archive fit experiment does not prove that eleven
declared targets plus any newly resolved baseline targets fit. Reduce portable
binary sizes only with parity evidence, or separately review an alternative
distribution contract; do not silently drop targets or increase bounds.

From `rust/`, the useful local commands are:

```sh
cargo xtask package-inspect TARGET PATH_TO_BINARY
cargo xtask package-inspect GNU_TARGET PATH_TO_BINARY --max-glibc 2.28
cargo xtask package --binary TARGET=PATH_TO_BINARY --output artifacts/rust-rewrite/npm-NEW --smoke
cargo xtask upgrade-rollback --native artifacts/rust-rewrite/npm-NEW/claude-autorouter-0.5.2.tgz --output artifacts/rust-rewrite/rollback-NEW
cargo xtask package --direct-from artifacts/rust-rewrite/npm-NEW/claude-autorouter-0.5.2.tgz --target TARGET --output artifacts/rust-rewrite/direct-NEW --smoke
```

`package`/`package-inspect`/`upgrade-rollback` paths resolve from the repository
root. Choose the real binary target and a fresh output directory. The GNU symbol
ceiling shown applies to the recorded tier-1/2 GNU baseline, not every
experimental port. These private host checks do not authorize publication.
Retain their exact archive hashes and source/test revision; repeat after code
changes. Real evaluator and Claude calls remain a separate explicitly invoked
qualification step.
