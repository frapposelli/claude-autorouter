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

This source snapshot is not yet linked into AutoRouter. Build-graph integration,
independent source auditing, exact artifact identities and both musl executions
remain pending. No terminal-restoration assertion is shortened or skipped.
