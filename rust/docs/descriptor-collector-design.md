# Descriptor collector follow-up

The [initial declaration](../parity/descriptor-benchmark-v1.json) records the
original design. Ordinary v4 macOS collection still uses bounded `lsof`; mount
warnings retain an unverified result. An explicit implementation selection is
described below. Existing reports and the v4 protocol remain unchanged.

A separately identified development helper can use the macOS SDK's
`proc_pidinfo` with `PROC_PIDLISTFDS`. It will read `PROC_PIDTBSDINFO` before and
after collection, require the owned live child's PID and start identity to
match, validate exact record alignment and unique nonnegative descriptor
numbers, and bound allocation to 131,072 SDK `proc_fdinfo` records. At most three
list attempts may handle growth; exhausted growth, full final buffers, permission
errors, exit, identity changes or malformed results remain unverified.

The helper needs only the existing C toolchain and SDK headers. Compilation and
the two-second bounded owned invocation occur outside measured HTTP intervals.
Its result contains only a fixed method/status, verification flag and count;
it never resolves or records descriptor targets, paths or raw diagnostics.
Source, binary, compiler and SDK identity must accompany fresh evidence.

This is a non-atomic OS snapshot, not a peak, an allocation measurement or a
long-term retention result. Linux collection and all existing workload,
concurrency, sample, resource and latency thresholds remain unchanged.

The [implementation companion](../parity/descriptor-implementation-v1.json)
adds an explicit development selection without changing ordinary v4 behavior:

```sh
ulimit -n 1024
cargo xtask benchmark --protocol v4 --workload tiny_strings_miss --descriptor-collector libproc-v1 --validate --output artifacts/rust-rewrite/fd-validation-NEW
```

On macOS this compiles the allowlisted C collector once before services start,
records compiler/SDK-header/source/helper identity, and uses it at all three
descriptor observation points. Linux retains `/proc` enumeration. Each helper
invocation has its own two-second deadline, bounded output and owned cleanup.
Diagnostics, invalid output or failure remain unverified. Validation removes
numeric descriptor observations; no descriptor recovery threshold or hardware
approval follows from successful controls. The original design declaration and
all historical lsof results stay unchanged.
