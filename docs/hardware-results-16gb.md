# Actual local evaluator measurements: 16 GiB M4

Measured October 5, 2026 using Ollama 0.35.0, Node v26.10.0 and an Apple M4 with 10 logical CPUs and 16 GiB unified memory. Applications remained running: initial 1/5/15-minute load averages were 3.55/3.35/3.48, and reported free memory was 842 MiB. Free memory is an OS snapshot, not total available memory or a measurement of swap pressure.

Each already-installed Q4_K_M candidate received one cold request, then three deterministic shuffled rounds of 30 held-out synthetic tasks, followed by eight separate full-excerpt stress tasks. The warm deadline was 30 seconds; the cold deadline was 60 seconds. The benchmark began with no resident Ollama models and unloaded only its own candidates afterward. No downloads, Jev requests or Claude inference occurred.

| Measurement | Tev1 4B Q4_K_M | Nimble 9B Q4_K_M |
| --- | ---: | ---: |
| Cold wall time | 7.13 s | 15.93 s |
| Warm requests / valid results | 90 / 90 | 90 / 87 |
| Warm wall p50 / p95, including errors | 244 / 2,930 ms | 1,250 / 13,645 ms |
| Successful warm wall p50 / p95 | 244 / 2,930 ms | 1,148 / 11,249 ms |
| Exact frozen-label agreement | 93.3% | 93.3% |
| Under-routes / over-routes | 3 / 3 | 3 / 0 |
| Deadline errors | 0 | 3 |
| Reported model residency | 2.71 GiB | 5.74 GiB |
| Reported context allocation | 2,050 tokens | 8,194 tokens |
| Full-budget stress p50 / p95 | 7,003 / 7,854 ms | 12,369 / 13,945 ms |

Both candidates classified tasks into all three tiers. Neither passed the preset strict gate of 100% agreement and zero under-routing. Tev1 classified one complex fixture as Sonnet in all three rounds and one Haiku fixture as Opus. Nimble classified one complex fixture as Haiku in all three rounds and timed out on three other requests. Both passed all eight stress labels, but the stress set contains only Haiku tasks and cannot establish broader classification accuracy.

These results support retaining model-specific deadlines and the explicit disabled-deadline option. They do not establish an optimal deadline, parity with Jev, downstream Claude task quality or subscription savings. Timings include local service work, model computation and scheduling under this background load; they are separate from the [synthetic router benchmark](router-performance.md). Differences in context allocation and model residency must be considered when comparing candidates.

[Machine-readable results](hardware-results-16gb.json) include per-case outcomes, model digests, token metrics, residency snapshots and background-load metadata. No user prompts or process names are included. The frozen fixture SHA-256 is `1ee6ab593e47d87a67e922c0afb0c9db5787d8a27d1a0ff3e24baa0543291fc5`; the rubric SHA-256 is `be151cedb4de4b7ef3f7162d751f70ce7d9dd14efc66fae1835f73ffd04027be`.

The same workload has now been measured on a 64 GiB M2 Ultra. See [the comparison](hardware-comparison.md) for the results and differences in model artifacts and runtime conditions. The Nimble timeout rows on this baseline recorded wall times exceeding their configured 30-second deadline, up to 969,208.59 ms; the cause was not established. The original observations and acceptance thresholds remain unchanged.
