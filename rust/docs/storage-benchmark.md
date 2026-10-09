# Synthetic storage tooling

`cargo xtask benchmark-storage --validate --output NEW_DIRECTORY` checks the real native status and session-log APIs using private temporary storage. Add `--implementation paired` to compare with the verified frozen Node modules. Native-only execution does not locate or run Node. Help, validation and historical report comparison make no evaluator calls, downloads, network connections or user-configuration changes.

Native validation also runs from the extracted benchmark source bundle and does not read or fingerprint the unused JavaScript oracle, reference verifier or baseline manifest. Paired mode retains those inputs, including the bundled verifier imported by the oracle, and requires Node plus a separately supplied frozen reference checkout. The bundle contains no frozen Node product modules.

The tool requires an explicit mode and refuses to overwrite evidence. The old JavaScript benchmark's arbitrary `--module PATH` option is replaced by `--native XTASK_EXECUTABLE` and, for paired execution, `--node NODE_EXECUTABLE --reference FROZEN_DIRECTORY`. The native executable here is the contributor `xtask`, not the product CLI. Historical report files remain read-only:

```sh
cargo xtask benchmark-storage --compare-status-legacy REPORT.json --label LABEL
```

That comparator preserves the four original recorded synchronous-baseline formulas and their comparison-before-display-rounding behavior. It accepts matching historical workload metadata only. It cannot approve native reports, compare against a missing baseline, or turn a descriptive measurement into an acceptance result.

Validation checks normal and held status writes, readiness and initialization failures, failed-write/rename recovery, concurrent flush/close, private files, exact final snapshots, correlated normalized log rows, metadata privacy, escaped UTF-8 queue accounting, 128-session capacity, continuous 800-row arrivals, append failures, and shutdown. A held storage operation must coexist with local task/timer progress; this is not an HTTP-stream or inference measurement. Shared drain behavior is the explicit Rust counterpart of JavaScript Promise identity. The Rust flush future is actually polled and retained, rather than timing lazy future construction. Failure wrappers delegate successful work to the real storage backend; a simulated append failure is not a kernel write-error test. Wrapper operation counts are not OS descriptor counts.

The frozen protocol is [storage-benchmark-v1.json](../parity/storage-benchmark-v1.json). Native-only semantic expectations are captured from the frozen Node modules and pin complete normalized semantic hashes. Only the declared timestamps, process IDs, random launch paths and object-key ordering are adapted; request identities, sequence order, models, usage, pricing, warnings and numbers remain checked. Reports retain source, protocol and executable hashes, immutable helper inputs, post-run identity verification, exact requested scenarios and cleanup. Partial failures remain failures and their evidence is retained.

Numerical collection is separately explicit:

```sh
cargo xtask benchmark-storage --measure --implementation paired --output NEW_DIRECTORY
```

Do not run this as ordinary validation. The paired profile declares five alternating matched rounds with 200 warmup and 2,000 measured bursts. The distinct `--profile legacy-status-workload` retains three repetitions of 60 bursts, six historical events per burst, 20 sessions and normal/20 ms delayed storage. Both use a 5 ms producer and progress timer. Full raw samples and method/environment conditions accompany descriptive results. The short historical profile does not support a strong p99 claim. The four historical synchronous-baseline gates are never applied to these native measurements.

Validation retains no numerical performance samples. Numerical mode has no ratified native acceptance threshold; `acceptance_qualified` remains false. These API workloads complement the HTTP benchmark without qualifying actual gateway interference, installed status polling, history at read limits, long resource soak, allocations, descriptor counts, representative hardware, provider quality or the complete performance gate. Product logging remains disabled by default.
