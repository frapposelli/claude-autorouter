# Observational i686 build provenance

The `i686-hosted-v1` profile records the existing Ubuntu 24.04 runtime32 job's
ordinary CLI build before its package helper is compiled. The collector runs on
the x86_64 host; the target is `i686-unknown-linux-gnu`. The existing `setarch`
runner and installed-artifact checks remain separate execution evidence.

This profile emits `portable_cpu: false`. Rust uses explicit Pentium 4 flags,
while native C and generated assembly still require qualification. The release
packer continues to require `portable_cpu: true`; this profile does not make
i686 a release candidate or establish an oldest OS, libc, kernel or CPU.

Both the host and explicit-target release directories must be absent before
the first production build. A top-level shell subshell scopes the admitted
flags around input collection, Cargo, feature inspection and pair creation.
It restores the original test environment before the runtime32 proof runs.
Later helper and final proof phases recheck the saved native pair independently
of the flat CLI copy.

The record binds source and Cargo configuration, actual host compiler tools,
installed target standard libraries, the exact source-owned test runner and
selected multilib suppliers. The APT inventory describes a rolling hosted
environment; it is not an immutable image or complete package-byte closure.
ELF inspection records actual version requirements without claiming the GNU
profiles' GLIBC 2.28 floor. Existing input, output, archive and deadline limits
remain in force. No downloads, builds or product execution occur in the collector.
