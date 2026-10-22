# Nix terminal storage correction

This is the complete published Nix 0.31.3 source with one initialization change in
`tcgetattr`: initialize the plain integer/array `libc::termios` storage before
calling libc. Keep all other source and the MIT license unchanged. The checksum,
upstream/patched file maps and exact patch are in `nix-provenance.json` and
`nix-termios-init.patch`.

Actual Linux musl CI at branch checkpoint `cffec6c` exposed unstable named control
character fields after slot 19. Both restoration tests failed while the first
19 control characters and terminal flags matched; the stopped-output child
completed rather than timing out. Nix calls libc on uninitialized storage, and
musl 1.2.5 delegates its getter to the legacy kernel ioctl.

Primary sources: [Nix 0.31.3](https://docs.rs/nix/0.31.3/src/nix/sys/termios.rs.html),
[musl getter](https://git.musl-libc.org/cgit/musl/tree/src/termios/tcgetattr.c?h=v1.2.5),
[Linux v6.17 terminal ABI](https://github.com/torvalds/linux/blob/v6.17/include/uapi/asm-generic/termbits.h).

The runtime and CLI use this source from immutable Git revision
`2286fd91d8165b4b56b273c2ebe5fb4f462ed672`. The bootstrap auditor uses the
unchanged registry Nix package without its terminal feature. Keeping the two
sources distinct lets the auditor check the full patched source before compiling
it. The compact source descriptor and patch travel in the source bundle;
Cargo fetches the complete reviewed supplier into its cache.

The independent native auditor verifies all 126 files, the MIT license, exact
patch and provenance, including reverse/forward reconstruction. It checks actual
locked Cargo identities, dependency edges, target paths and compiled features;
the same-version registry and Git packages cannot substitute for one another.
The current host bootstrap/source checks and 28 auditor tests pass. Ordinary
release artifact inspection, both musl executions, final source transfer and
complete platform/release qualification remain pending. No terminal-restoration
assertion is shortened or skipped.
