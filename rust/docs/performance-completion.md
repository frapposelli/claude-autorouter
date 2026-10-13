# Rust rewrite performance completion audit

This audit retains the scope of [the rewrite plan](../../docs/rust-rewrite-plan.md#performance-strategy-and-acceptance).
It separates a working measurement capability from successful measurements and
from acceptance. A green fixture test, an old source checkpoint or a single-host
ratio does not qualify the final native distribution. The unchanged thresholds
are in [performance-gates.json](../parity/performance-gates.json).

Audited on 2026-10-13 before any protocol 4 numerical measurement. The shipping
reference remains `ea930c247626ce2af5ccdad721b5121417bf4ad8` (JavaScript 0.5.2).
Do not overwrite older reports, pool protocol versions or relax gates to turn a
failure into acceptance.

| Requirement | Current authoritative evidence | What still proves completion |
| --- | --- | --- |
| Cold/warm help, version and status; at least 50% lower median startup | Four retained [v1 exploratory comparisons](performance.md); driver starts complete processes and compares stdout | Final-candidate five matched rounds on representative systems; cold OS-cache conditions, installed npm dispatcher/native archive, synthetic snapshot and 40/80/100/120 columns are still absent |
| Ready-to-serve, Jev and Ollama warmup | HTTP driver verifies health and records readiness for mock Jev | Equivalent Ollama startup/warmup workload and installed-launcher boundary; v3/v4 readiness includes collector overhead and is descriptive |
| Idle RSS at least 40% lower | Same-host v1 observed ratios; complete-process `ps` samples | Final-candidate representative runs under recorded power/load conditions |
| Large-request/concurrent peak RSS at least 30% lower | [v3 validation](../parity/measurements/local-v3-validation-summary.json) verifies the isolated `getrusage` collector, not a numerical result | Five new verified paired lifetime peaks for each fixed large-catalog concurrency; retain exact scope, confidence intervals and the unchanged 0.7 threshold |
| CPU/request at least 30% lower | Four v1 comparisons retain earlier failures; the fourth five-round run observed targets on that checkpoint | Final-candidate and full declared workload matrix on representative hardware; interval CPU resolution must remain sufficient, not replaced with lifetime CPU |
| Large-catalog processing-only p95 at least 50% lower | [Matched component protocol](../parity/processing-benchmark-v1.json) implements fresh parse/validate/real-router/prepare/serialize helpers with immediate injected transport and exact output/call/decision checks | New numerical component results under the unchanged 0.5 target plus proof for excluded HTTP pre/postprocessing; helper alone cannot qualify the whole shipping gateway. End-to-end HTTP percentiles and subtraction of independent control percentiles are insufficient |
| No important p95/p99/throughput regression | v1 fourth run detects no regression for its limited matrix and source checkpoint | All declared final-candidate workloads, five paired rounds, confidence intervals, 10% relative allowance and 0.5 ms noise floor; throughput minimum 0.9 |
| Full response latency, first body byte, stalls and byte throughput | [v2 declaration](../parity/local-benchmark-v2.json) and deterministic exact-response checks implement burst/paced SSE | Numerical v2-or-later runs; UTF-8 fragmentation, compressed pass-through, slow client, long stream and failure/cancellation timing remain absent |
| Near-limit request body and tiny-string parser memory | [v4 declaration](../parity/local-benchmark-v4.json) and driver fixtures add 32 MiB minus 4096 bytes and 65,536 independent one-character strings | New matched numeric results with exact request/count fields and unchanged bounds; no historical large-tool workload qualifies the per-string memory tradeoff |
| Descriptor counts and postload recovery | v4 bounds `/proc/PID/fd` enumeration or PID/f-only `lsof`; initial/warm/postload outcomes are retained | Verified measurements on each representative OS; warnings/truncation remain unverified. Three non-atomic snapshots have no peak or long-term-retention claim |
| Allocations, retained records and long resource recovery | Product bounds have deterministic tests; HTTP measurements contain null allocation/retention fields | Equivalent allocation accounting, a declared long mixed-task/capacity soak, repeated launch/kill cycles, bounded retention and recovery reports |
| Cache hit/miss/expired and credential/config isolation; identical/distinct agent requests | Fixed cache-hit/miss and agent-ID modulo concurrency fixtures exist | Workloads for expiry, same-agent and distinct-agent shared evaluations, isolation and capacity, with exact required call counts |
| Pinned/new/goal continuity, count-token and cancellation stages | Separate deterministic routing/gateway suites; initial benchmark only incidentally performs speculative count calls | Explicit timing/resource workloads for each named state and each cancellation stage, including independent shared waiters and zero-deadline cancellation |
| 429/5xx/SSE error/truncated/malformed/timeout replies | Deterministic transport tests exist | Matched workload measurements retaining statuses, failures and cleanup, without successful-response assumptions |
| Slow/failed storage, status polling, history at read limits | [Storage protocol](storage-benchmark.md) implements native/reference storage tools but explicitly disclaims numerical acceptance | Actual gateway interference and installed renderer/history workloads; history schemas/read limits/truncation/symlink rejection; storage/resource/cleanup measurements |
| Real evaluator and completed-task quality | Existing quality failures remain retained; synthetic policy parity is separate | Matched opt-in baseline/candidate runs under predeclared variability, unchanged rubric/fixtures/thresholds and exact evaluator identity; separately report availability, under-routing, policy, tier coverage, transport and independently verified task completion |
| Compiler/runtime choices and portable release optimization | Thin LTO/codegen-units1 and one current-thread native executor are in source; no native CPU target | Empirical representative-hardware comparison where required; exact compiler/profile/binary/source identity. No alternate allocator/PGO/SIMD claim without profiling and parity evidence |

## Measurement boundaries

Versions 1–4 launch the frozen Node CLI and native executable against identical
driver-owned synthetic services. Their HTTP timing includes network, mock
serialization/validation and driver scheduling. Direct-upstream controls use
the same fixtures; controls do not authorize subtracting independent p95 values
or calling an HTTP result processing-only latency.

Protocol 3's fresh single-child collector records full-lifetime maximum
individual resident memory, including waited descendants, after child reaping.
It excludes driver/mocks/collector/previous workloads and is not summed process
tree RSS or measured-interval peak. Lifetime CPU stays descriptive and distinct
from interval CPU. Versions 1–3 and prior failed outcomes remain immutable.

Protocol 4 inherits the exact v3 declaration by SHA-256, requires new matched
runs, and adds gateway-only descriptor/recovery observations outside the timed
interval. It records omitted families when `--workload` is used. A filtered
run never qualifies the full workload protocol or plan. Validation does not
retain numeric performance/resource results.

## Required representative conditions

Real local-evaluator trials require at least a normal-use 16 GiB Mac and a Mac
with more than 24 GiB RAM, with exact CPU/OS/architecture, RAM, power mode,
background load, Ollama runtime/model tag and digest, residency and cold/warm
conditions. A larger developer workstation cannot represent the smaller
memory class. Linux/WSL and supported distribution targets still need their
relevant installed/resource behavior; a macOS numerical run makes no Linux
claim. Keep real inference/model quality separate from gateway overhead.

Record the settings before each candidate, pause agent builds/tests during
numerical runs, and keep source archive, executable hash, compiler/build log,
fixture/protocol hashes and all raw samples. Near-limit inputs at 128 clients
can occupy several GiB in the gateway and external driver; a full five-round
measurement moves several TiB through loopback. Retain an OOM, deadline or
collection failure as failure rather than selecting easier concurrency,
increasing a deadline or discarding observations after the result is known.

The overall performance gate remains **incomplete** until this table is proved
by final-candidate evidence across the required scope. Implementation of a
collector, a protocol declaration or a report that states `completed: true`
only proves that tool's named operation, not rewrite completion.
