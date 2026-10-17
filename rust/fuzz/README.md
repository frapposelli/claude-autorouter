# Opt-in sanitizer fuzzing

This separate workspace exercises the native JSON arena, redaction, response
observer parser and bounded history reader. It is excluded from ordinary Cargo
checks and product dependencies. It makes no network requests, launches no
providers and reads no user configuration or history. All checked seeds are
synthetic. Generated corpora and failure artifacts remain local.

The targets check these properties:

| Target | Oracle |
| --- | --- |
| `json_document` | Canonical round trips, failed-edit nonmutation, exact inserted value and preservation of other fields including lone-surrogate keys. |
| `redaction` | Arbitrary lossy-UTF8 input does not panic; an appended recognized synthetic Authorization field never leaks its token. An existing unlabeled occurrence of that token is excluded from the field-specific assertion. |
| `response_observer` | Whole versus fragmented input produces identical observations; destruction prevents further callbacks. This tests the parser, not transport forwarding or downstream completion. |
| `session_history` | Record/line bounds, byte accounting, supported schemas/events and single-session isolation; summaries remain renderable. |

Each target rejects inputs larger than 65,536 bytes. Observer/history targets
interpret the first three bytes as bounded options. `seeds.json` identifies
every checked seed, including valid records, malformed input, deep JSON,
surrogates and inputs at the size boundary. This is a starting corpus, not a
coverage or completeness claim.

From `rust/`, install the pinned tools into an isolated directory. This requires
network access once and does not change the user's default Rust toolchain:

```sh
fuzz_tools="$PWD/../artifacts/rust-rewrite/fuzz-tools"
cargo install cargo-fuzz --version 0.13.2 --locked --root "$fuzz_tools/cargo-fuzz"
env RUSTUP_HOME="$fuzz_tools/rustup" rustup toolchain install nightly-2026-10-08 --profile minimal
cargo fetch --manifest-path fuzz/Cargo.toml --locked
node fuzz/verify.mjs
```

Build explicitly with AddressSanitizer. Keep the target directory separate from
ordinary development builds and verify that the fuzz lockfile stays unchanged:

```sh
env -u CARGO_ENCODED_RUSTFLAGS -u RUSTFLAGS \
  RUSTUP_HOME="$fuzz_tools/rustup" RUSTUP_TOOLCHAIN=nightly-2026-10-08 CARGO_NET_OFFLINE=true \
  "$fuzz_tools/cargo-fuzz/bin/cargo-fuzz" fuzz build \
  --fuzz-dir fuzz --target-dir "$PWD/../artifacts/rust-rewrite/fuzz-target" \
  --sanitizer address --codegen-units 16
node fuzz/verify.mjs
```

The verifier pins the exact fuzz lock bytes, checks shared dependency sources
and checksums against the product lock, and checks every seed. Running it before
and after building detects lock mutation. Clearing both compiler-flag variables
prevents inherited flags from overriding cargo-fuzz's instrumentation. Retain
the target, core and runtime compiler fingerprints to verify the actual flags.

The recorded campaigns instrument the targets and their Rust dependencies with
coverage, ASan and debug assertions. They use the prebuilt standard library;
native C dependencies are not separately sanitizer-instrumented. The 0.13.2
help text describes `--build-std` as default-on, but its implementation defaults
to false. To instrument the standard library in a separate experiment, install
`rust-src` into the isolated toolchain and explicitly pass `--build-std`.

For a bounded campaign, copy checked seeds into a fresh directory, then run the
corresponding built executable. Replace `TARGET_TRIPLE` and `TARGET_NAME` below
with the compiler target and one of the four names above:

```sh
fuzz_run="$PWD/../artifacts/rust-rewrite/fuzz-new-run"
```

Keep the following sequence guarded so a failed directory creation stops before
any existing corpus or log can be overwritten:

```sh
(
set -e
mkdir "$fuzz_run"
mkdir "$fuzz_run/failures"
cp -R fuzz/seeds/TARGET_NAME "$fuzz_run/corpus"
ASAN_OPTIONS=detect_odr_violation=0 \
  ../artifacts/rust-rewrite/fuzz-target/TARGET_TRIPLE/release/TARGET_NAME \
  "$fuzz_run/corpus" -seed=20261009 -runs=100000 -max_total_time=60 \
  -timeout=5 -rss_limit_mb=1024 -max_len=65536 -print_final_stats=1 \
  "-artifact_prefix=$fuzz_run/failures/" > "$fuzz_run/fuzz.log" 2>&1
)
```

Use a fresh run directory to preserve earlier evidence. Where available, set
`ASAN_SYMBOLIZER_PATH` to an LLVM symbolizer and record its version/hash. An
outer process watchdog should retain the log and failure directory if the
fuzzer itself hangs. The recorded runs use a 90-second outer deadline.

Retain executable/source/lock/toolchain hashes, exact arguments, initial and
final corpus inventories, exit status and all failure artifacts. Reproduce a
failure by passing its file to the same binary. Check an invariant against the
frozen Node contract before treating an assertion as a product defect. Reduce
and retain genuine failures as deterministic regressions; never discard a
failure merely to obtain a green report. Passing finite campaigns does not
establish all-input safety, transport parity or production memory performance.

[provenance.json](provenance.json) records the five added registry dependencies,
published archive checksums, license files and exact libFuzzer sources. Its
LLVM notice retains the discrepancy between the crate's NCSA metadata and
bundled source headers. These tools and licenses are development-only.
The [campaign summary](../parity/measurements/fuzz-local-summary.json) records
the completed host runs and their limits. See the upstream
[cargo-fuzz guide](https://rust-fuzz.github.io/book/cargo-fuzz/tutorial.html)
for corpus management and minimization.
