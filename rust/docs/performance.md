# Native performance investigation

The fourth five-round comparison meets the measured CPU, throughput and latency
nonregression targets on this workstation, including the previously failing
large-catalog workload. The rewrite's overall performance gate remains
incomplete: true peak memory, processing-only latency, additional workloads and
representative hardware are still unmeasured. Earlier failed runs remain below.

## First comparison

The first five-round comparison completed 225 rows with matching synthetic
request, evaluator, and token-count calls. It did **not** meet the rewrite's
performance gates. The [retained summary](../parity/measurements/local-v1-summary.json)
contains every row's aggregate metrics, paired confidence intervals, unchanged
thresholds, raw-report hash, and immutable source/binary build evidence.

The developer workstation was an Apple M2 Ultra with 64 GiB RAM, macOS 27,
Node 22.14.0, and Rust 1.99.0. Both gateways used one primary event-loop thread.
The reference source was commit `ea930c247626ce2af5ccdad721b5121417bf4ad8`.
The candidate source archive SHA-256 was
`4ed88737c92495d02634129f91648f86e1dcfa9fb074e940dfe4c4aca5ce3728`.
This snapshot predates the optimizations described below.

| Observation | Native / Node ratio |
| --- | --- |
| Help/version median process startup | About 0.09 |
| Status renderer median process startup | About 0.15 |
| Idle whole-process RSS | About 0.31 |
| Small-request CPU per request | 0.62–0.67 across concurrency groups |
| Large-catalog CPU per request | 2.71–2.96 |
| Large-catalog throughput | 0.30–0.33 |
| Cache-hit throughput | 0.62–0.71 |

Large-catalog latency regressed at every measured concurrency. Cache-hit
p95 regressed at concurrency 8, 32, and 128. Startup and idle-memory targets
were observed on this host, but the uncontrolled hardware conditions prevent
representative-hardware acceptance. HTTP latency includes network and mocks;
100 ms RSS samples cannot establish true peak memory. The complete workload
matrix, resource soak, allocations, and real-evaluator evidence remain open.

## Profiling and subsequent changes

Five-second macOS `sample` captures identified repeated tool-catalog parsing,
JSON serialization, metadata projection, and software SHA-256 as hot paths.
These separate diagnostics ran during development and establish no timing
improvement. They used 1,000 sequential requests and a token-count mock of
200,001, whereas the declared benchmark uses 1,000; their outcomes must not
be substituted for the benchmark's measurements.

The initial changes remove three full metadata projections used only to read
small routing fields, and avoid reparsing serialized turn-identity inputs.
Object fragments use JavaScript property ordering directly. A frozen Node
hash regression covers numeric keys, cache markers, lone surrogates, negative
zero, and overflowing numbers. The full deterministic differential suite
passed after those routing changes.

The JSON parser and serializer subsequently gained explicit ASCII paths.
RustCrypto SHA-256 now enables its `asm` feature, which selects ARM SHA
instructions at runtime when supported and retains a software fallback.
No workstation-specific CPU target is used. The complete differential suite
passed before the second immutable benchmark snapshot was built.

## Second comparison

The [second retained summary](../parity/measurements/local-v1-run-2-summary.json)
contains all 225 aggregate rows, paired intervals and the raw-report hash.
All expected request/evaluator/count calls and response bytes matched.
The candidate was built offline from detached commit
`05a5f700812e4427a1b6a667ecd45b19bc6614ce`, using the same machine, Node version,
Rust version, five-round ordering, sample counts and mocks as the first run.
The source archive SHA-256 is
`c70b77d792acde4c3f0c8844cf0bbfff8ae3a2c4e6664df49547e41e7a098c52`.
The native executable SHA-256 is
`543bf65f5109515dfa30ac23fd9efd05749e47185dcfd0a6c33265d826645da3`.

| Observation | Native / Node paired-round median ratio |
| --- | --- |
| Help/version startup | 0.091–0.094 |
| Status renderer startup | 0.151 |
| Idle whole-process RSS | About 0.177 |
| Small-request CPU per request | 0.336–0.344 |
| Small-request throughput | 1.82–2.07 |
| Cache-hit CPU per request | 0.456–0.474 |
| Cache-hit throughput | 1.40–1.58 |
| Large-catalog CPU per request | 1.01–1.12 |
| Large-catalog throughput | 0.789–0.822 |

Large-catalog throughput remains below the 0.9 floor at every concurrency, and
CPU remains above the 0.7 target. Its p95/p99 regression is established at
concurrency 32; other large-catalog latency intervals are inconclusive under
the unchanged noise floor. Cache-hit p95 at concurrency 128 is also
inconclusive. Small-request latency checks detect no regression.

These are same-host exploratory observations, not representative-hardware
acceptance. The processing-only p95 target, true peak RSS, allocations and the
remaining workloads are still unmeasured. The snapshot also predates the
subsequent correction to certificate-chain verification; these results do not
qualify that later executable.

## Changes after the second comparison

Profiling still identified string serialization in the large-catalog path.
JSON strings now share immutable storage when documents are cloned, retain
ordinary scalar text as UTF-8, and materialize UTF-16 only when an operation
needs code units. Strings containing lone surrogates retain their exact
UTF-16 representation. Parsing and quoting copy ordinary text spans without
converting every character individually. Each immutable string also records
whether it requires JSON escaping, avoiding repeated scans during serialization.
The same 1,000-request diagnostic profile no longer identified quoting as the
dominant sampled function; this is profiling evidence, not a timing result.

The 17-family differential suite passed all 206,625 cases after this change,
including escaped keys, all UTF-16 code units, opaque provider fields, and
JavaScript number semantics. These checks establish correctness within that
corpus. The third immutable comparison below measures this change together with
the corrected certificate verifier.

Shared storage has a memory tradeoff: each string carries reference-count and
cache metadata, and escaped scalar strings can retain both UTF-8 and UTF-16.
Inputs dominated by tiny strings may therefore use more memory than before.
The initial large-catalog workload does not qualify that case; it requires a
separate measured workload before making a general memory-improvement claim.

## Third comparison

The [third retained summary](../parity/measurements/local-v1-run-3-summary.json)
contains every aggregate row and paired interval from the unchanged five-round,
225-row protocol. All request, evaluator and count calls and response bytes
matched. The offline build used detached commit
`c58df75e3753f0df5d957243e0f98eb792c6bc16`; its source archive SHA-256 is
`c32e6a820ce432d5cbecfaac23798e2ce89d151553e1484a71d3458ae0f91bee` and
native executable SHA-256 is
`e143efa31e5e230ee26345b271eb295c1ad4caa6c3e1b36e098a3804aa93de49`.
Local agent builds and tests were paused during measurements. The hardware,
runtime/compiler versions, sample counts and thresholds match the earlier
protocol; the workstation remains uncontrolled and results are exploratory.

| Observation | Native / Node paired-round median ratio |
| --- | --- |
| Help/version startup | About 0.058 |
| Status renderer startup | About 0.097 |
| Idle whole-process RSS | About 0.176 |
| Small-request CPU per request | 0.352–0.359 |
| Small-request throughput | 1.80–2.00 |
| Cache-hit CPU per request | 0.467–0.493 |
| Cache-hit throughput | 1.31–1.53 |
| Large-catalog CPU per request | 0.581–0.620 |
| Large-catalog throughput | 1.39–1.43 |

Every measured HTTP workload's p95/p99 interval detected no regression under
the declared allowance. Large-catalog CPU and throughput now meet the 0.7 CPU
ceiling and 0.9 throughput floor. Its concurrency-one HTTP p95 ratio is 0.686;
that includes transport and mock overhead and does not establish the separate
0.5 processing-only target. The response timing records when headers are ready,
not the first body byte. Sampled RSS does not establish true peak memory.
No result qualifies later source edits, unmeasured workloads, other hardware,
live providers, or production cutover.

The [measurement protocol](../parity/local-benchmark-v1.json) and
[acceptance gates](../parity/performance-gates.json) remain unchanged. Raw
samples, source archive, build log, and profiler captures are retained under
`artifacts/rust-rewrite/` in the development checkout; the checked-in summary
does not claim that those local files are published release evidence.

## Fourth comparison

The [fourth retained summary](../parity/measurements/local-v1-run-4-summary.json)
records one unchanged v1 run on the same M2 Ultra host: five alternating rounds,
225 rows, and matching response bytes and evaluator/upstream/count calls. It
took about 11.6 minutes. The measured checkpoint19 executable SHA-256 is
`6ce97bbf7d093867e3d15ac4bfed3c0ca44e2ddd7b84a6c38d63109c3d2cda3c`;
the retained optimized driver is `8bd9ae6d…`. No tuning or retry followed the
results, and earlier failures remain retained.

Help/version startup was about 2.7 ms native versus 48–49 ms Node; status startup
was 2.76 versus 30.0 ms. Median idle RSS was about 10.1 MiB versus 58 MiB.
The following ratios are paired-round medians across the declared concurrency
groups; the delayed workload uses concurrency one only.

| Workload | Native / Node CPU per request | Native / Node throughput |
| --- | --- | --- |
| Small request | 0.356–0.362 | 1.75–1.94 |
| Cache hit | 0.469–0.494 | 1.31–1.51 |
| Large catalog | 0.571–0.621 | 1.41–1.44 |
| Small request, 5 ms mock delay | 0.442 | 1.04 |

All 42 measured ratio targets were met, and all 26 p95/p99 checks detected no
regression under the unchanged thresholds. Large-catalog concurrency-one HTTP
p95 was about 3.65 ms native versus 5.45 ms Node; this does not establish the
separate processing-only target. True peak memory, streaming performance,
allocations, resource soak, other hardware and live quality remain unqualified.

The binaries were built with pinned Rust 1.99.0. The raw environment report's
Rust 1.81.0 is the subsequently observed executable on `PATH`, not the build
compiler. A separate deadline-under-backpressure fix was drafted during this
run without builds or tests; it is absent from the measured immutable binary.
These results support the checkpoint19 candidate's local opt-in performance
case and do not qualify that later fix, publication or default cutover.

## Completion audit and protocol 4

The [performance completion audit](performance-completion.md) maps the original
plan requirements to retained evidence and distinguishes implemented collectors
from measurements and acceptance. The existing four numerical comparisons are
protocol 1 exploratory runs. Protocols 2 and 3 have functional validation; their
stream and kernel lifetime-resource collectors still need fresh paired numerical
runs on the final candidate and representative hardware.

The [v4 declaration](../parity/local-benchmark-v4.json) adds exact near-limit
requests and many tiny opaque strings at concurrency 1/8/32/128. It records
gateway-only descriptor snapshots and sampled memory after load. Descriptor
targets and raw diagnostics are never retained; failed or ambiguous collection
cannot establish recovery. A single short postload snapshot is descriptive,
and does not replace a long soak or allocation/retained-state measurements.

Select it with `cargo xtask benchmark --protocol v4 --output NEW_DIRECTORY
--validate`. The optional `--workload near_limit_body_miss` or
`--workload tiny_strings_miss` runs one family at all four concurrency levels
and records excluded coverage. Validation strips numerical resource/timing
values. Numerical runs require an optimized driver, five new matched rounds
and a quiet machine; near-limit concurrency can consume several GiB and the
full measurement moves several TiB over loopback. Versions 1–3, their failures
and acceptance thresholds are unchanged. No numerical v4 result is claimed by
the declaration or its deterministic tests.

Protocol 4 checks that the command's soft file-descriptor limit is at least
1,024 before starting services; the direct 128-client control alone needs more
than 256 handles. In bash/zsh, use `ulimit -n 1024` in the dedicated benchmark
shell before the command. Both engines inherit and record the same limit. The
initial tiny-string validation's incorrect count-call expectation and later
256-handle control failure remain retained in separate evidence directories.

Full v4 and the near-limit family also require at least 32 GiB known physical
RAM before services start. This safety heuristic reflects the roughly 4 GiB
raw-input floor at 128 clients; it does not prove available headroom or peak
memory. Refused runs remain unmeasured. Small and tiny-string families can
still run on 16 GiB using `--workload`, and their hardware qualification is
still required. Run the complete near-limit family on the larger Mac without
reducing its declared concurrency.

The [v5 declaration](../parity/local-benchmark-v5.json) adds a private synthetic
online status snapshot and stdin fixture at 40, 80, 100 and 120 columns. To
validate only these renderer variants on a 16 GiB host, use a current native
CLI binary smaller than the driver's 32 MiB executable-input limit:

```sh
cargo xtask benchmark --protocol v5 --workload startup_status_snapshot \
  --validate --rust PATH_TO_NATIVE_CLI --output NEW_DIRECTORY
```

Each variant checks exact synthetic output bytes against the frozen Node CLI;
the selected report names the inherited HTTP and earlier startup workloads it
did not run. Validation strips timing samples. A full, unfiltered v5 run still
inherits v4's 32 GiB near-limit and 1,024-descriptor preflights. Neither
validation nor a normal-use 16 GiB timing run approves the unchanged startup
target or representative-hardware gate.

The [processing component protocol](../parity/processing-benchmark-v1.json)
provides a separate matched parse/validate/real-router/prepare/serialize boundary
in fresh Node and native development helpers. It uses the full classifier and
token-counter implementations with immediate in-memory response transports,
checks exact evaluator/count payloads and decision metadata, and includes
speculative count work before ending a sample. Run `cargo xtask benchmark
--processing --output NEW_DIRECTORY --validate` for deterministic validation.
Its declared numerical comparison retains the original 0.5 p95 target, five
alternating rounds and 2,000 samples per round. HTTP receive/authentication,
response observation, storage and downstream delivery are excluded. The
component result therefore cannot independently approve the whole gateway
processing gate or the shipping executable; its report keeps those limits.

For the larger Mac, `cargo xtask benchmark-bundle --transfer NEW_DIRECTORY`
creates an allowlisted Rust source archive, a separately verified frozen Node
reference archive, checksums and a resume guide. Their combined expansion
retains the existing 32 MiB source budget. The default bundle stays Rust-only.
The transfer contains no executables, Cargo cache, credentials or private
reports; the destination builds the pinned release tools and uses Node only
for the temporary reference comparison. Regenerate it from the final source
checkpoint before collecting qualification evidence.

## Buffered turn fingerprints

The turn fingerprint writer now feeds exact JSON fragments into SHA-256 through
a 4 KiB batch buffer instead of assembling a catalog-sized serialized preimage.
It shares the authoritative JSON writer and preserves cache-control
normalization, property order, prompt aliases and task history. Traversal stacks
still allocate in proportion to structure; this is not a constant-memory claim.
Twenty-six synthetic frozen Node cases retain complete preimages and keys,
including a normalized preimage exceeding the batch size and escaped scope and
prompt identities.

Two fresh whole-connection experiments used the unchanged five-pair protocol:

| Recorded helper | Median native/reference p95 ratio | 95% interval | Unchanged 0.5 gate |
| --- | --- | --- | --- |
| Before buffered fingerprints | 0.495 | 0.469–0.677 | Inconclusive |
| Buffered fingerprints | 0.469 | 0.396–0.576 | Inconclusive |

All functional, call, completion and cleanup controls match in both runs. They
ran sequentially on the normal-use 16 GiB M4 with changing background load, so
the difference does not establish an improvement caused by the code change.
The [source and evidence summary](../parity/measurements/native-turn-fingerprint-summary.json)
retains the exact measured helpers and later validation separately. Neither
experiment qualifies representative hardware, the final source or a shipping
artifact, and the full performance gate remains incomplete.
