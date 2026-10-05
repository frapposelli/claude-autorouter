# Synthetic router performance

On October 5, 2026, eight simultaneous identical requests made one evaluator call after coalescing, compared with eight before it. This holds across different agent scopes. Distinct requests still evaluate independently, and pinned tool continuations still evaluate on every request. [The recorded baseline and candidate](router-performance.json) contain three rounds of measurements and their comparison gates.

The baseline was recorded before changing routing or evaluator transport. The final candidate was measured at 10:25 UTC after the compatibility guards and response handling were frozen; the baseline and comparison thresholds were retained unchanged. Both versions used the same synthetic harness: 60 iterations per scenario, concurrency eight, a mocked evaluator with a one-millisecond timer, and a 286,807-byte request containing 300 synthetic tools. No network services, user prompts, provider credentials, model inference, or downloads were involved.

Measurements came from one 16 GiB Apple M4 Mac with ten logical CPUs, Darwin 25.6.0, and Node v26.10.0. The workstation remained in normal use. Its one-minute load average was 2.32 after the baseline and 2.22 after the final candidate; free memory and the remaining load averages are in the JSON. The final run preceded the full regression suite to avoid interference from that workload. No user process names or arguments were collected.

## Results

The table reports the largest p50 and p95 across the three rounds, in milliseconds. Counts exclude setup calls that warm the cache or establish the initial pin.

| Scenario | Before p50 / p95 | After p50 / p95 | Evaluator calls per round, before → after |
| --- | ---: | ---: | ---: |
| Small request | 1.331 / 1.646 | 1.261 / 1.396 | 60 → 60 |
| Large tool catalog | 3.421 / 4.415 | 2.810 / 3.037 | 60 → 60 |
| Cached request | 0.025 / 0.035 | 0.010 / 0.014 | 0 → 0 |
| Pinned tool continuation | 1.373 / 1.778 | 1.261 / 1.346 | 60 → 60 |
| Eight identical concurrent requests | 1.936 / 2.815 | 1.915 / 2.397 | 480 → 60 |
| Identical requests from eight agents | 1.715 / 2.219 | 1.566 / 1.815 | 480 → 60 |
| Distinct requests from eight agents | 1.686 / 2.201 | 1.523 / 1.907 | 480 → 480 |
| Immediate caller cancellation | 0.097 / 0.124 | 0.028 / 0.056 | 60 → 0 |

The evaluator-call reduction is deterministic. Timing differences also reflect timer scheduling, garbage collection, JIT warmup, and background load; these measurements do not establish faster Claude responses or a model inference speedup. Route time includes the synthetic evaluator delay and JSON processing. `policy_overhead_ms` separately reports route timing minus classification timing; it is an approximate measure using the router's rounded telemetry. There is no network or real model latency in either measure.

The JSON includes retained heap/RSS changes, sampled peak heap/RSS changes, and event-loop delay. Sampling runs every millisecond, with explicit garbage collection before and after each workload. It can miss allocations inside one synchronous interval. The event-loop histogram has ten-millisecond resolution, so values near ten milliseconds are the sampling floor. RSS is descriptive because allocator page reuse can dominate differences between short runs.

## Local regression gates

For each scenario, the p95 route-time limit is twice the largest p95 across the three pre-change rounds. Peak sampled heap and p95 event-loop delay use the same baseline-relative margin. For example, the small-request route limit is 3.292 ms, the large-catalog limit is 8.831 ms, and the identical-agent limit is 4.439 ms. These are local regression guards, not portable latency promises or CI timing thresholds. The comparison also requires matching workload parameters, Node version, and hardware, and exactly one evaluator call per group of eight identical requests. All recorded gates pass.

Repeat this process with a new output file before and after a proposed change:

```sh
node --expose-gc scripts/benchmark-router.mjs baseline /tmp/router-comparison.json
# Apply the proposed change.
node --expose-gc scripts/benchmark-router.mjs candidate /tmp/router-comparison.json
```

The script refuses to replace an existing baseline and exits nonzero when a comparison gate fails. Its results should be reviewed alongside deterministic concurrency/cancellation tests. Actual local-model measurements on 16 GiB and 64 GiB Macs are recorded separately in [the hardware comparison](hardware-comparison.md), including classifier failures and comparability limits. Status-file storage performance is measured separately.

## Bounds and preserved behavior

The router retains at most 256 pending evaluations and 1,024 subscribers. Capacity exhaustion uses the conservative fallback and reports `classifier_error: capacity_exhausted`. Each subscriber can cancel independently; the final cancellation aborts the evaluator, removes its pending entry, and prevents a late answer from entering the cache. Failed work is never cached. Coalescing shares only classification; each caller still applies its own scoped turn, context, and compatibility policy.

Keys retain the full request body, including the requested model/floor, and include the evaluator endpoint, model, credentials, deadline, excerpt budget, confidence policy or keep-alive setting, and a frozen rubric hash. Only their SHA-256 digests are stored as keys; credentials and request contents are not benchmark output. Using a smaller excerpt-only key would need a separate compatibility review.

Both evaluators use the same bounded JSON reader: 64 KiB for decisions and 1 MiB for local-model metadata. Streamed bytes enforce the limit even with missing or inaccurate length headers. A single growable buffer bounds chunk metadata, malformed JSON produces a stable error category, and cancellation never waits on an uncooperative body-cancellation promise. Jev's deadline now includes body reading; Ollama's configured zero deadline remains disabled while caller cancellation and byte limits remain active.

Prior-pin classification reuse was considered and intentionally deferred. The benchmark shows approximately 1.3 ms median total routing time for synthetic pinned turns, while eliminating the evaluation would alter the existing evaluate-per-request behavior. Coalescing removes duplicate simultaneous evaluations without making that semantic change. New human tasks and sequential tool continuations retain their current evaluation and safety checks.
