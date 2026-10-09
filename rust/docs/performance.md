# Native performance investigation

The third five-round comparison meets the measured CPU, throughput and latency
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
