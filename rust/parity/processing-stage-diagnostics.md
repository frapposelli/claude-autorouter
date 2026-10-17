# Matched processing stage diagnostics

Run `cargo xtask benchmark --processing --diagnose-stages --output NEW_DIRECTORY` from `rust/` to inspect one synthetic Node/Rust pair. The mode fixes eight warmup and 32 measured requests. It uses the same large-catalog fixture, immediate in-memory evaluator and token counter, request identities, output hashes, call-count checks, 1 MiB control bound, 10-second per-sample advisory wait, and 30-minute child limit as the [frozen processing benchmark](processing-benchmark-v1.json). No provider or model is called. The output directory must be new.

Each `raw_stage_ms` series measures wall time between consecutive marks in one helper process:

| Field | Start → end |
| --- | --- |
| `parse` | Immediately before request-body parse → parsed object/document available |
| `validate` | Parsed object/document available → request-shape validation complete |
| `router` | Validation complete → awaited real router call returns |
| `prepare` | Router returns → model-specific request preparation returns |
| `serialize` | Preparation returns → prepared request serialization complete |
| `advisory_settle` | Serialization complete → required count response and call-count checks complete |

The counter may run concurrently with `router`, `prepare`, and `serialize`. These intervals are observational wall times, not exclusive CPU costs or a causal decomposition. Timer reads and diagnostic branches also perturb the measured path. The new helper/source hashes differ from historical runs, so do not pool them or claim longitudinal comparability. Compare stages to find a profiling target, then verify any optimization with a new uninstrumented run under the unchanged benchmark protocol. Do not sum stage medians or use the diagnostic output as performance acceptance evidence. The report deliberately contains no threshold, interval, or passing ratio; it retains exact deterministic parity evidence and declares `acceptance_qualified: false`.
