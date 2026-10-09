# Native performance investigation

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
No workstation-specific CPU target is used. These later changes require
fresh differential checks and a new immutable benchmark snapshot before
claiming a speedup or a passing gate.

The [measurement protocol](../parity/local-benchmark-v1.json) and
[acceptance gates](../parity/performance-gates.json) remain unchanged. Raw
samples, source archive, build log, and profiler captures are retained under
`artifacts/rust-rewrite/` in the development checkout; the checked-in summary
does not claim that those local files are published release evidence.
