# Rust differential characterization

This directory freezes the JavaScript reference and records the remaining rewrite work. Passing its current pure fixtures establishes those cases only. It does not establish complete gateway, CLI, packaging, performance, privacy or provider parity.

`baseline.json` records SHA-256, Git blob identity and size for the 111 baseline runtime, tool, test, fixture, package and CI files at `ea930c247626ce2af5ccdad721b5121417bf4ad8`. The Node adapter verifies every file before importing any baseline source. It never loads saved user configuration, inherited credentials, an evaluator or a provider. `xtask` materializes these immutable Git objects into `artifacts/rust-rewrite/reference` and reuses that verified snapshot; existing files are never overwritten or silently refreshed. The baseline commit must exist locally (CI fetches it). Use `--reference-root` for another unchanged checkout. No worktree is reset or edited.

`coverage.json` inventories each literal baseline test definition, the behavior areas, configuration keys and resource bounds. Every full area remains pending until its detailed assertions have retained Rust evidence. Parameterized test names are source templates; they do not replace the baseline execution count. Case-to-suite links identify origins, not proof that an entire suite has been ported.

[evidence.json](evidence.json) maps implemented components to source files, declared Rust tests and content-addressed copies of local differential/installed-package reports. It retains report hashes and observed counts for the 588 literal baseline definitions. [Individual assertion mappings](assertion-mappings/config-auth.json) record reviewed test definitions, exact shared cases, evidence hashes, and any remaining assertions; unreviewed definitions remain pending in `coverage.json`. Related component evidence is deliberately distinct from approval of a whole baseline test or behavior area. This index does not rewrite historical reports; each retained report copy is addressed by its content hash.

Run from the Rust workspace so the pinned toolchain is selected:

```sh
cd rust
cargo xtask freeze-reference
cargo xtask parity --report artifacts/rust-rewrite/parity.json
cargo test --locked --package xtask
```

Paths passed to `xtask` resolve against the repository root. `--cases PATH` selects another synthetic JSONL corpus. `--reference-root PATH` selects the unchanged baseline checkout. `--candidate PATH` invokes another fixture executable with the `fixture` subcommand. Omitting it evaluates the compiled Rust modules directly.

Each input is `{ "id": "unique-case", "op": "operation", "input": {} }`. Additional `source_tests` fields record provenance. Each response repeats `id` and `op` and has exactly one `result` or `error`. `cargo xtask fixture` and `node scripts/rust-reference.mjs` read this protocol on stdin and emit JSONL on stdout. Duplicate/empty/malformed cases, missing or reordered results, failed or stalled executables and unimplemented operations cannot become a passing comparison. Reports show case IDs and differing JSON pointers, excluding request/result payloads. JSON object order is ignored in results; strings, array order, null/missing and distinct large integers are not normalized away. Cache/hash serialization gets separate explicit fixtures when implemented.

Operations cover model facts, request validation/adaptation, compatibility/safeguards, explicit-environment configuration, deterministic turn-state schedules, redaction, telemetry, savings, prompt state, routing, evaluation reports, status rendering, history and bounded response observation. These are newly authored synthetic cases derived from baseline tests. The clock fallback for telemetry is fixed to `2026-10-09T12:00:00.000Z`; valid supplied timestamps remain unchanged. Configuration paths resolve against the logical project root even when the reference source lives in a separate snapshot. Inputs are bounded to 8 MiB per line and 64 MiB per run; maximum-size gateway tests need a separate executable transport harness. The `js_json`, raw request and observer fixtures exercise JavaScript UTF-16 strings, number serialization, key ordering and deep opaque JSON explicitly; ordinary JSON fixtures alone cannot establish those contracts. The observer corpus now includes consumed lone-surrogate model identities without replacement-character normalization.

Check reference identity independently from the repository root:

```sh
node scripts/rust-reference.mjs --root artifacts/rust-rewrite/reference --check-baseline
```

`performance-gates.json` retains the plan's thresholds. `benchmark-protocol.json` inventories the complete workload matrix; `local-benchmark-v1.json` freezes the initial driver settings before measurement. The native driver starts each complete executable against identical local mock services. Its initial subset covers warm-filesystem process startup, small requests, a 300-tool catalog, cache hits, a fixed evaluator delay and concurrency 1/8/32/128. It records five alternating paired rounds, 200 warmup and 2,000 measured HTTP samples per run, raw samples and paired bootstrap intervals. The 0.5 ms latency noise floor and all missing metrics remain visible; unimplemented workloads and uncontrolled hardware cannot pass the overall gate. True peak RSS, isolated gateway processing time, cold disk-cache startup and real evaluator performance are not inferred from these measurements.

`cargo xtask benchmark --compare-legacy REPORT.json` reads a bounded historical `{ "baseline": ..., "candidate": ... }` report and prints the frozen JavaScript comparator result without running measurements. The eight captured calls in `benchmark-router-contracts.jsonl` preserve complete results, scenario order, workload/environment checks, maximum-of-rounds limits, row counts and evaluator-call ratios. Empty candidate maxima retain their original JSON `null` representation. This compatibility mode cannot qualify the native paired protocols or transfer-bundle format.

The opt-in `--protocol v2` selects `local-benchmark-v2.json`: the same initial workloads plus successful burst and paced SSE forwarding. Version 1 remains the default and its prior evidence stays intact. Version 2 records response-header and first-nonempty-body latency separately, exact response hashes and lengths, driver-observed maximum data-frame gaps and byte throughput. It compares bytes incrementally across arbitrary HTTP fragmentation and rejects truncation, changed bytes, unexpected trailers or missing body observations. Producer counters establish that mocks yielded their complete bodies; the driver separately requires the complete bytes through EOF. Neither observation substitutes for the gateway's transport acknowledgement tests. Five fresh matched rounds are required for comparisons; `--validate` retains only functional evidence. Stream cancellation, backpressure, failure timing and the other missing resource workloads remain open.

The opt-in `--protocol v3` adds a fresh collector for each HTTP gateway's full-lifetime peak RSS and CPU usage. The gateway and collector share a process group whose leader remains unreaped in the driver until cleanup. A complete report preserves the gateway's exit status; the collector then deliberately terminates its group with SIGKILL, and the driver verifies that protocol and cleans the still-owned group before accepting a row. This also permits driver cleanup of a stopped collector. Responsive collectors handle driver loss; simultaneous abrupt driver death and an unresponsive collector are outside the portable guarantee. RSS is the largest individual process peak among the child and its waited descendants, not aggregate process-tree or measured-interval memory. All five peak-memory pairs must have verified collection; numerical values alone cannot qualify a pair. `--validate` retains no numerical resource results. Earlier validation reports retain their original protocol hashes; this cleanup refinement predates numerical v3 measurements.

```sh
# Deterministic protocol/count/byte validation; no retained numerical timings.
cargo xtask benchmark --validate --output artifacts/rust-rewrite/benchmark-validation-NEW
# Opt-in local measurements; use a quiet machine and a fresh release binary.
cargo build --release --locked --package claude-autorouter
cargo run --release --locked --package xtask -- benchmark --output artifacts/rust-rewrite/benchmark-NEW
```

Each destination must be new. The driver writes its environment, protocol, source/binary hashes and unchanged thresholds before starting, then retains each completed row and any failure. It uses synthetic credentials, private temporary configuration, loopback upstream/evaluator URLs and no model downloads. It reports external whole-process RSS and CPU resolution rather than V8 heap; 100 ms RSS samples are only a lower bound on peak memory. Experimental evaluator quality failures remain separately visible; a language port does not justify changing labels, models or rubrics to hide them.

Run every retained pure-function/observer family with one deterministic command:

```sh
cd rust
cargo xtask parity --all --output artifacts/rust-rewrite/parity-all-new
```

`--output` is resolved relative to the repository root, so use
`artifacts/rust-rewrite/parity-all-new` when choosing a repository-local path.
Omitting it creates a unique timestamped directory. An existing destination is
rejected. The suite retains each generated corpus, input/source/report hashes,
individual reports, and `suite.json`; it continues after a mismatch to expose
all failing families. It includes exhaustive UTF-16 and floating-point probes,
compatibility, routing, response observation, privacy, accounting, prompts,
configuration/authentication, status, history, and evaluation report logic.
This command does not run HTTP executable comparisons, installed-archive
lifecycle tests, timing measurements, or live evaluators.

The [raw HTTP/1 pool experiment](measurements/openssl-raw-pool-summary.json)
uses a test-only OpenSSL client and a default-off vendor reservation feature.
Its independent driver compares 63 synthetic schedules over HTTP, TLS 1.2 and
TLS 1.3 against Node 22.14.0. It checks actual peer socket ownership alongside
pool probes, LIFO reuse, the 256-idle limit, timeout hints, busy/idle timers,
session resumption and cleanup. A reference-only or native-only run is
characterization, not a differential result. The summary retains failed
drafts and mutation controls, as well as the separate broader transport
comparisons that still differ. Clean response consumption and pool retirement
do not establish successful downstream forwarding. The ordinary native
release remains Rustls, with both experimental vendor features absent.

The auto-routing baseline also has a dynamic contract capture. It verifies the
frozen source hashes, runs all 23 original test callbacks and assertions, and
records every JSON-safe guard call with its result and before/after request
serialization. The native fixture invokes the document-based guard used by the
runtime. Two non-finite-number API calls remain explicit migration boundaries;
they are not silently converted to null and counted as covered.

```sh
node parity/capture-auto-routing-contracts.mjs ../artifacts/rust-rewrite/reference ../artifacts/rust-rewrite/auto-routing-contracts-NEW
cargo xtask parity --cases artifacts/rust-rewrite/auto-routing-contracts-NEW/cases.jsonl --report artifacts/rust-rewrite/parity-auto-routing-contracts-NEW.json
```

The retained capture identifies every baseline definition and dynamic case.
Capturing the reference alone is not native evidence; the second command must
pass against the same corpus. This supplements the broader generated guard
corpus and does not establish complete gateway routing or provider behavior.

The pure history fixture defaults explicitly to `en-US`; actual environment
selection is tested separately. History count ordering uses the native ICU4X collator and preserves JavaScript
integer-key enumeration and stable insertion order for collation ties. The
locale corpus compares explicit ordered-key arrays and terminal text, so object
comparison cannot hide an ordering mismatch. The frozen Node 22.14.0 oracle uses
ICU 76.1 / CLDR 46.0; pinned ICU4X 2.3.1 embeds CLDR 48.2.1. Version differences
remain visible in the retained locale reports. `check-history-locales.mjs`
separately compares actual CLI behavior under isolated `LC_ALL`, `LC_MESSAGES`,
and `LANG` settings.

The frozen router capture runs all 51 original definitions and 222 assertion
sites, preserving 121 Router instances, 263 routes and 29 pure helper calls.
Its lossless string dictionary verifies every reconstructed request hash.
Native core tests replay policy; separate runtime tests execute the actual
classifier, cache and token counter against captured synthetic transport
responses, checking complete requests, decisions and call totals. Only the
injected turn-state clock is fixed; evaluator deadlines and classifier cache
TTL retain their original clock behavior. Four definitions remain partial for
the wall-clock deadline assertion, original count-callback argument shape,
non-JSON callback return types and synchronous callback-start ordering.
The [router assertion mapping](assertion-mappings/router-contracts.json) retains
those boundaries and exact evidence hashes.

```sh
node parity/capture-router-contracts.mjs ../artifacts/rust-rewrite/reference ../artifacts/rust-rewrite/router-contracts-NEW
cargo test --locked --package autorouter-core --test router_contracts
cargo test --locked --package autorouter-runtime router::contracts
```

The [concurrency mapping](assertion-mappings/router-concurrency-contracts.json)
replays all 10 frozen schedules and 55 assertion sites (93 observations), with
independent deadlines around complete native schedules. It includes the full
256-work and 1,024-subscriber limits. Four definitions are covered; six retain
explicit JavaScript cancellation, eager-promise, mutable-configuration and
stream/timer boundaries. Captured call-expression hashes exclude the trailing
semicolon; canonical definition hashes include it.

The [evaluation-report mapping](assertion-mappings/evaluation-report-contracts.json)
covers 15 of 16 definitions. Native tests compare complete reports and actual
evaluator requests across both profiles, outages and collapsed tier coverage.
Only the three unasserted timing fields are excluded. The remaining definition
records nonfinite inputs, frozen-object mutation and factory identity as API
boundaries; unmeasured quality gates remain unmeasured.

The [setup and diagnostic mapping](assertion-mappings/local-setup-diagnostic-contracts.json)
adds nine finite error transcripts and bounded warmup, pull and residency
cancellation schedules. Two definitions are covered and four remain partial
for JavaScript signal/reason observations. The separate
[finite setup mapping](assertion-mappings/local-finite-setup-contracts.json)
covers six more definitions with 24 original operations and 63 requests,
including minimum versions, model aliases and warmup progress. The
[finite diagnostic mapping](assertion-mappings/local-diagnostic-contracts.json)
covers seven more definitions across ten scenarios, comparing all 102 requests,
58 progress events and complete reports. It excludes only 48 specifically
unasserted elapsed-time observations; presence, type and nonnegative values
remain required. Formatter tests compare the raw captured timing values without
exclusions. Nine other setup/diagnostic definitions remain pending. These
synthetic tests make no local-model timing or quality claim.

The [finite Ollama evaluator mapping](assertion-mappings/ollama-evaluator-finite-contracts.json)
covers five definitions across 15 scenarios, comparing complete task states,
requests and results. The [Ollama router mapping](assertion-mappings/ollama-router-contracts.json)
covers five more definitions with 26 scenarios and 79 fetches through the
actual shared classifier and router. Each original instance keeps its cache
and retry sequence. The oversized metadata fixture retains its full input;
the production metadata limit stays at 1 MiB. Deadline, cancellation and
continuity definitions remain separate.

`ollama-routing-contracts.jsonl` retains 26 original Router instances and 29
routes across four model tags and six Claude-shaped fixtures. Native replay
compares all 58 requests and complete decisions, including successive human
prompt identities and signed-thinking preservation. The two unasserted elapsed
fields are checked for finite, nonnegative values before comparison. Selected
models remain unconfirmed selections; these fixtures do not simulate successful
provider execution.

`ollama-routing-tool-contracts.jsonl` exercises six synthetic runs of the local
routing-test tool, including warmup, collapsed classifications, invalid answers,
duplicate prompts and unsafe preflight. Its native tool API preserves setup
error codes; CLI diagnostics continue to print the safe message. Progress output
includes the classifier error when present. Clock fields and fixture-hash API
differences are explicitly recorded in the capture rather than counted as live
model, timing or quality evidence.

`savings-finite-contracts.jsonl` preserves 47 trackers, 421 updates, 104 original
snapshot reads and 21 saved-outcome estimates from 15 complete callbacks.
Native tests compare complete intermediate totals and outcomes, then mutate
detached snapshots to check state isolation. Numeric comparisons use exact
JavaScript Number values without tolerance; integral and floating JSON spellings
of the same number are equivalent. Omitted own-undefined fields and native map
isolation are explicit API adaptations. Prices and their recorded provenance
remain the frozen baseline; capacity, hostile accessors and pricing-fact
immutability have separate obligations.

```sh
node parity/capture-ollama-routing-contracts.mjs ../artifacts/rust-rewrite/reference ../artifacts/rust-rewrite/ollama-routing-NEW
node parity/capture-ollama-routing-tool-contracts.mjs ../artifacts/rust-rewrite/reference ../artifacts/rust-rewrite/ollama-routing-tool-NEW
node parity/capture-savings-finite-contracts.mjs ../artifacts/rust-rewrite/reference ../artifacts/rust-rewrite/savings-finite-NEW
cargo test --locked --package autorouter-runtime --test ollama_routing_contracts
cargo test --locked --package xtask ollama_routing::contracts
cargo test --locked --package autorouter-core --test savings_finite_contracts
```

The separate `ollama-routing-timeout-contracts.jsonl` retains the original
four-route sequence on one Router: legitimate Sonnet, timeout fallback, fresh
Haiku recovery and an adaptive-thinking compatibility guard. Native replay uses
the actual evaluator deadline with a controlled clock, verifies the pending
request is dropped and compares all eight requests and complete results. The
source mock observes its expired 5 ms signal after a 20 ms delay; this establishes
the original output contract, not equal elapsed time or scheduler behavior.

`savings-capacity-contracts.jsonl` preserves 1,127 original updates and nine
original snapshots across the two in-flight and session-eviction callbacks.
Complete snapshots check partial/unpriced accounting, late evicted work and new
requests in returning sessions. Additional native checks exercise the exact
1,000-request and 100-session boundaries. These are state-capacity checks, with
no process-memory or timing claim.

`statusline-boundary-contracts.jsonl` compares 124 complete renderer outputs
from ten original callbacks. Each explicitly recorded native value conversion
also produces the same complete output in the frozen renderer: omitted own
undefined properties, invalid top-level values, four nonfinite numeric examples,
observed liveness and the capture process ID. This is limited to the captured
values and the native pure-renderer API. Full ANSI output, model qualifiers,
session isolation, guards and width priorities remain exact. The 216 executed
original assertions include 76 of 77 static sites; the remaining conditional
site has a false predicate for all 21 original tiny-width inputs, which native
replay checks explicitly. No execution of that untaken branch is claimed.

```sh
cargo test --locked --package autorouter-runtime --test ollama_routing_timeout_contracts
cargo test --locked --package autorouter-core --test savings_capacity_contracts
cargo test --locked --package autorouter-core --test statusline_boundary_contracts
```

`local-diagnostic-timeout-contracts.jsonl` retains the two original disabled and
finite runtime-deadline callbacks, including 48 requests and 30 progress events.
The disabled case checks the original `latency_ms >= 4` predicate against real
elapsed time before comparing reports. The finite case lets the production
2 ms deadline cancel six delayed valid responses and checks that fallback cannot
count as a Sonnet classification. Additional controlled-clock tests retain the
separate 60-second preparation bound and caller cancellation with no runtime
deadline. Exact raw formatter output is preserved; elapsed values are not a
hardware performance result. Rust's cancellation-token API is an explicit
library adaptation of JavaScript signal identity.

```sh
cargo test --locked --package autorouter-runtime --test local_diagnostic_timeout_contracts
```

The [redaction mapping](assertion-mappings/redaction-contracts.json) compares
93 complete pure-call results and covers twelve functional definitions.
Two more definitions remain partial: their 28 timing assertions have not been
executed or converted into performance claims.
Constructed synthetic credentials use lossless JSON escapes in the captured
corpus; decoded fixture values and expected results match the frozen source.

The [bounded JSON mapping](assertion-mappings/bounded-json-contracts.json)
checks exact UTF-8 byte limits, malformed input, stalled cancellation and
owned-body cleanup. One definition is covered and four remain partial for
JavaScript reader, reason-identity and cancellation-promise observations.
The [configuration/Keychain mapping](assertion-mappings/config-keychain-contracts.json)
covers four command definitions using private temporary files and an injected
memory Keychain, including migrations, locked storage, failed writes and
redacted provenance. It requires the system policy path to be absent and does
not modify it. The [additional Keychain mapping](assertion-mappings/keychain-extra-contracts.json)
covers secret edits through real isolated stdin and verifies that session
history never reads the Keychain. Its adapter remains an in-memory fixture.
The [setup/command mapping](assertion-mappings/keychain-setup-contracts.json)
covers four more definitions using private platform/prompt adapters and exact
unspawned `/usr/bin/security` command construction. It does not qualify actual
Keychain access. The [empty-history mapping](assertion-mappings/history-empty-contracts.json)
covers three real inspection commands with disabled logging or a missing
directory, including `ENOENT`, no file creation and a forbidden secret store.

The [server response mapping](assertion-mappings/server-response-contracts.json)
covers three definitions and 22 static assertions through actual synthetic
downstream and upstream HTTP. It checks token-count errors, local rejections,
and exact compressed bytes with response observation bypassed. The input
extractor evaluates frozen synthetic initializers and gzip construction;
separate unchanged Node tests provide the original callback controls.
The [subscription mapping](assertion-mappings/server-subscription-contracts.json)
covers four more definitions and 24 assertion sites: all six original rejected
credential combinations, exact 401/429 provider responses without API-key
fallback, OAuth model discovery/token counting, and turn-scoped system
messages. It also checks event privacy after normal gateway cleanup. These
finite loopback tests do not qualify live subscription accounts or transport
timing.

The [release-tool mapping](assertion-mappings/release-contracts.json) covers
15 definitions and leaves four partial. Five original pure callbacks produce
61 captured calls; native tests also replay synthetic registry and installer
schedules. JavaScript private-error injection, independent clock injection
and native publication eligibility remain explicit boundaries. The
[remaining release mapping](assertion-mappings/release-remaining-contracts.json)
covers two source/tag definitions and records twelve partial archive/entrypoint
definitions. Native archive admission, historical package format, diagnostics
and installed source-root selection remain explicit differences. A corrupt
checksum now retains the original public report reason, `invalid_archive`,
while the reader keeps its specific checksum detail. These tests do not publish
a package or qualify an actual release.

```sh
node parity/capture-router-concurrency-contracts.mjs ../artifacts/rust-rewrite/reference ../artifacts/rust-rewrite/concurrency-NEW
node parity/capture-evaluation-report-contracts.mjs ../artifacts/rust-rewrite/reference ../artifacts/rust-rewrite/evaluation-report-NEW
node parity/capture-local-setup-diagnostic.mjs ../artifacts/rust-rewrite/reference ../artifacts/rust-rewrite/local-contracts-NEW
cargo test --locked --package autorouter-runtime router::concurrency_contracts
cargo test --locked --package autorouter-core --test evaluation_report_contracts
cargo test --locked --package xtask evaluation_contracts
cargo test --locked --package autorouter-runtime --test local_setup_diagnostic_contracts
node parity/capture-local-finite-contracts.mjs ../artifacts/rust-rewrite/reference ../artifacts/rust-rewrite/local-finite-NEW
node parity/capture-release-contracts.mjs ../artifacts/rust-rewrite/reference ../artifacts/rust-rewrite/release-contracts-NEW
cargo test --locked --package autorouter-runtime --test local_finite_contracts
cargo test --locked --package autorouter-runtime --test bounded_json_contracts
cargo test --locked --package claude-autorouter configuration_keychain_contracts
cargo test --locked --package xtask release_contracts
node parity/capture-local-diagnostic-contracts.mjs ../artifacts/rust-rewrite/reference ../artifacts/rust-rewrite/diagnostic-NEW
node parity/capture-server-response-contracts.mjs ../artifacts/rust-rewrite/reference ../artifacts/rust-rewrite/server-response-NEW
cargo test --locked --package autorouter-runtime --test local_diagnostic_contracts
cargo test --locked --package autorouter-runtime --test server_response_contracts
cargo test --locked --package xtask release_pack::tests::archive_contracts
```
