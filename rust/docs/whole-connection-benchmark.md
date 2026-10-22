# Whole gateway processing probe

The [immutable protocol](../parity/whole-connection-benchmark-v1.json) measures
the ordinary HTTP/1 gateway with immediately available synthetic transport. It
includes authentication, body assembly, JSON validation, real routing and count
processing, request preparation, header construction, genuine inference client
serialization/parsing, response observation, downstream framing and actual
delivery-based continuity. Node uses its frozen server connection listener and
`http.request`; Rust delegates to the same `Gateway::serve_connection` as its
ordinary listener, with the same pinned Hyper-util client builder and policies.
Both downstream inputs are immediately available in at most 8 KiB chunks.

Each pair compares the complete non-clock decision/lifecycle data, meaningful
headers, exact prepared/provider bytes, evaluator/count payloads and accepted
continuation evidence. Both engines also repeat an ordinary TCP loopback
control, and hold a downstream writer/flush after model observation and count
completion to prove that the sample/commit does not finish prematurely. Detailed
hashing and response verification run after the processing clock stops.

The [frozen policy branch](../parity/whole-connection-policy-v1.json) is asserted
explicitly for each request. Jev classifies Sonnet, but the compatible-profile
large-context downgrade guard retains the source Opus 5.5. The genuine earlier
Haiku count returns 1,000 tokens; that advisory result does not clear the guard
for the selected native-million-context Sonnet target. Both engines verify the
actual parsed return, count target, classified tier, selected/inference model
and reason. This workload measures a classified request whose compatibility
guard preserves its model. Earlier protocols and validation reports stay
unchanged; this companion declaration predates numerical collection.

Evaluator/count external networking uses the same declared supported immediate
mocks as the processing protocol. Their real payload/header construction,
bounded parsing and count-cache/finally work remain included. DNS, TCP, TLS,
provider/model latency and filesystem persistence are excluded. Internal
runtime/event-loop scheduling and bounded synthetic socket/collector bookkeeping
remain included. Ordinary loopback durations are controls, never subtracted
from an independently measured percentile.

Validate first from `rust/`:

```sh
cargo xtask benchmark --whole-connection --validate --output artifacts/rust-rewrite/whole-validation-NEW
```

Validation is the default when neither mode flag is supplied. It exercises one
pair with 8 warmup and 32 checked requests, retains source/helper/Node/adapter and
fixture hashes, and stores no processing clocks. All failures are preserved;
choose a fresh directory rather than overwriting earlier evidence.

The [boundary review](../parity/whole-connection-benchmark-review-v1.json)
records the original successful controls and authorizes a separate numerical
interface. Explicit collection requires release helpers:

```sh
cargo run --release --package xtask --bin xtask --locked --offline -- benchmark --whole-connection --measure --output artifacts/rust-rewrite/whole-measurement-NEW
```

This runs five alternating Node/native pairs, 200 warmup and 2,000 measured
requests per engine per pair, with fresh loopback/held-writer controls. It uses
the unchanged 0.5 candidate/baseline p95 target, nearest-rank distributions and
5,000 seeded paired bootstrap resamples. It records power/load/toolchain and
source/binary identities, every individual synthetic completion proof and raw
sample, without outlier removal. A normal-use machine supplies exploratory
evidence; controlled representative hardware and shipping artifact qualification
remain separate requirements. The tool never automatically approves those or
the overall rewrite gate.

The `autorouter-runtime/development-probes` feature is off by default and enabled
only by `xtask/runtime-tools`. It exposes the ordinary connection owner to bounded
development I/O and a client with an injected connector; it adds no inference
endpoint or permission/authentication behavior. Build shipping executables with
the ordinary package-specific command, and use the native production feature
auditor to reject this feature in the actual release artifacts. The independent
`autorouter-audit` bootstrap remains free of the inspected runtime and its
development connector dependencies.

Reports from this helper are not installed-binary performance results. Regenerate
the source transfer after the final source freeze; retain the existing 32 MiB
expanded transfer budget and all required source/fixture inputs.
