# Local evaluator comparison: 16 GiB M4 and 64 GiB M2 Ultra

The October 5, 2026 measurements now cover both required memory classes. The returned 64 GiB report contains valid cold, warm and full-budget observations from a Mac in normal use with the usual applications open. Both candidates completed every request there, but both still failed the unchanged classification gate. Jev remains the default; these local measurements do not establish production routing accuracy.

The frozen workload is one cold request, three deterministically shuffled rounds of 30 held-out synthetic tasks, and eight separate 3,000-byte stress tasks per model. Warm and cold deadlines were 30 and 60 seconds respectively. The benchmark uses native `/v1/systemone` choice scoring, with no Claude generations, Jev requests or model downloads. The reported latency is end-to-end wall time, not isolated model computation.

| Measurement | Tev1 4B, 16 GiB | Tev1 4B, 64 GiB | Nimble 9B, 16 GiB | Nimble 9B, 64 GiB |
| --- | ---: | ---: | ---: | ---: |
| Cold wall time | 7.13 s | 3.57 s | 15.93 s | 3.77 s |
| Warm requests / valid results | 90 / 90 | 90 / 90 | 90 / 87 | 90 / 90 |
| Warm wall p50 / p95, including errors | 244 / 2,930 ms | 55 / 519 ms | 1,250 / 13,645 ms | 56 / 863 ms |
| Successful warm wall p50 / p95 | 244 / 2,930 ms | 55 / 519 ms | 1,148 / 11,249 ms | 56 / 863 ms |
| Exact frozen-label agreement | 93.3% | 93.3% | 93.3% | 96.7% |
| Under-routes / over-routes | 3 / 3 | 3 / 3 | 3 / 0 | 3 / 0 |
| Warm deadline errors | 0 | 0 | 3 | 0 |
| Full-budget stress p50 / p95 | 7,003 / 7,854 ms | 1,122 / 1,314 ms | 12,369 / 13,945 ms | 1,940 / 2,003 ms |
| Strict warm classification gate | Failed | Failed | Failed | Failed |

Tev1 under-routed `held-o-compiler` from Opus to Sonnet in all three rounds and over-routed `held-h-literal-opus` from Haiku to Opus. Nimble under-routed `held-o-injection` from Opus to Haiku in all three rounds. The 16 GiB Nimble run also had three timeout errors, which count against its agreement. Both models reached all three tiers and passed all eight stress labels on both hosts. Those stress tasks all expect Haiku; passing them does not establish broader accuracy. The preset warm gate remains 100% exact agreement, zero under-routing and all three classified tiers.

On the larger Mac, Tev1's stress p95 was 1.31 seconds and Nimble's was 2.00 seconds. The zero-error results were obtained with a 30-second warm deadline and do not certify reliability under a 1.5-second deadline. These observations support configurable model-specific deadlines, including the explicit disabled-deadline option. They do not determine an optimal deadline or justify changing the existing defaults.

## Conditions and comparability

| Condition | 16 GiB baseline | 64 GiB returned run |
| --- | --- | --- |
| Processor / logical CPUs | Apple M4 / 10 | Apple M2 Ultra / 24 |
| Unified memory | 16 GiB | 64 GiB |
| OS release / architecture | 25.6.0 / arm64 | 27.0.0 / arm64 |
| Node / Ollama | v26.10.0 / 0.35.0 | v22.14.0 / 0.35.1 |
| Initial 1/5/15-minute load | 3.55 / 3.35 / 3.48 | 3.87 / 5.12 / 4.93 |
| Final 1/5/15-minute load | 2.23 / 3.60 / 3.67 | 4.49 / 5.09 / 4.94 |
| Initial / final OS free memory | 842 / 122 MiB | 727 / 136 MiB |
| Background use | Applications running | Normal use, usual applications open |

Both hosts reported Q4_K_M quantization, 4.2B/9.0B parameters, context allocations of 2,050/8,194 tokens and model residency of 2.71/5.74 GiB for Tev1/Nimble. Residency is reported by Ollama; free memory is an OS snapshot, not total available memory or measured swap pressure. The JSON also records aggregate RSS across matching Ollama daemon/runner processes, which is not a pure model footprint. On the larger host, that aggregate rose from 2.94 to 7.99 GiB for Tev1 and 5.80 to 9.42 GiB for Nimble.

Model tags match but artifact digests differ:

| Model tag | 16 GiB digest | 64 GiB digest |
| --- | --- | --- |
| `tev1:4b-q4_K_M` | `3509ac7180e86e5fa8efc7b5745e32d55dd9d4e0a86bc9a88aba5323a5d29bc6` | `07be32e6e5e3dcc6b2deae7c68b89321e99daeedb08521e92771cc155473cbd9` |
| `nimble:9b-q4_K_M` | `3776806da5587387a996e28e75d5d07fbebe7410d47879e1f71782f36c896ce3` | `572f1f4c801d37171344b0520d14853a88887c249d11db3acfa6c81a34cdcfa3` |

Each returned download size is 28 bytes larger. This does not establish whether weights, templates or metadata changed. The comparison is observational: processor, runtime versions, background conditions and model artifacts differ, so faster timings cannot be attributed to RAM alone. A controlled hardware comparison would require matching model digests and runtime conditions. The 16 GiB timeout rows also recorded wall times exceeding their configured 30-second deadline, up to 969,208.59 ms; their cause was not established, so the timer should not be described as a strict wall-time upper bound in that run.

## Evidence and validation

[The 16 GiB report](hardware-results-16gb.json) and [the 64 GiB report](hardware-results-64gb.json) retain all per-case outcomes and unchanged gates. The larger report is complete while its overall `passed` field remains `false`, correctly reflecting failed classification gates. An independent offline review recomputed case/round coverage, labels, UTF-8 state sizes, percentile summaries, confusion matrices and acceptance gates.

All 36 hashes in the returned source manifest matched the frozen transfer sources when received. The fixture SHA-256 is `1ee6ab593e47d87a67e922c0afb0c9db5787d8a27d1a0ff3e24baa0543291fc5`; both model rubric hashes are `be151cedb4de4b7ef3f7162d751f70ce7d9dd14efc66fae1835f73ffd04027be`. Subsequent documentation and package-allowlist changes do not change those evaluated sources. The original transfer bundle is retained unchanged.

The returned report SHA-256 is `b76d25b3b062821fa2483e13847d79267734f1a83b35ffe6ea85c57f82f7f715`; its manifest SHA-256 is `dab377757912028fca5a929dd4ef2123a4c4001559f0d4ee26fd6bec092b04f1`. The public JSON includes this provenance and the user's background-use description; it contains synthetic case identifiers and metadata, without user prompts or process names. Reproduction instructions remain in [hardware-benchmark.md](hardware-benchmark.md).

The larger-memory measurement requirement is fulfilled. Classifier errors remain visible rather than being hidden by altered labels or thresholds. Neither this comparison nor the separate [router benchmark](router-performance.md) establishes parity with Jev, completed Claude task quality or subscription savings.
