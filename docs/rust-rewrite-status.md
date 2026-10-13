# Rust rewrite completion audit

This checklist tracks the full [rewrite plan](rust-rewrite-plan.md), including its cutover phase. It does not redefine completion as an experimental candidate, four convenient platforms, or a passing subset of tests. The frozen baseline is `ea930c247626ce2af5ccdad721b5121417bf4ad8` / JavaScript 0.5.2. Status reviewed on 2026-10-13; changes in progress require fresh validation before approval.

## Completion criteria and authoritative evidence

| Plan requirement | Evidence required | Current state / next work |
| --- | --- | --- |
| One native executable, every product command, no JavaScript sidecar | Installed command/JSON/exit-code matrix, Node-free execution and binary/build feature inspection | Implemented experimental candidate; host tests pass. Complete supported-platform execution remains open. |
| Keep package/command names, npm global installation and direct archives | Exact npm and per-target archive identities, scripts-disabled offline installation, installed byte/mode checks and lifecycle execution | Native npm and direct host feasibility pass the 74 installed lifecycle checks on the recorded initial binary. Fresh-source and production release-set qualification remain open. |
| Retain all thirteen behavior areas | Individual original-assertion mappings, shared fixtures, Rust tests and explained interface differences | Coverage ledger: 514 covered, 74 partial and no pending definitions after the original server, producer, protocol, rubric, compatibility, permission-overlay, prompt-order, savings and setup-input reviews. Partial entries still contain genuine missing behavior and explicit native API/format migrations; none is blanket approved. Reviewed native API differences are recorded per assertion. Do not infer whole-area approval from fixture counts. |
| Transparent provider responses, signed history, safety and permissions | Actual HTTP/SSE/JSON/compression/framing comparisons, client disconnects, failed flushes and pipelined-response ownership | Substantial synthetic coverage; unresolved transport/TLS differences and unreviewed original HTTP schedules remain. |
| Authentication, refreshed subscription credentials and custom-upstream restrictions | Request-scoped credential tests, API/OAuth/beta rejection matrix, conservative unsupported-contract behavior | Implemented and tested on synthetic transports; live canaries and supported-platform TLS qualification remain. |
| Configuration, policy, Keychain and history without mandatory migration | Exact lexical identities, permissions/revision/fsync/no-follow checks, policy admission, isolated migration/rollback and existing schemas/prices | Isolated real macOS Keychain migration passes all 25 host assertions through both engines. Complete assertion review, failure/rollback and platform qualification remain. |
| Bounds, cancellation and state ownership | Full boundary/interleaving matrix; bounded retention, no uncancelled workers, clean shutdown, no false continuation confirmation | Many deterministic proofs exist. Every inventoried configuration key/limit and full failure/resource matrix must be reviewed. |
| Predeclared speed and memory gates | Fresh matched five-round reports on representative hardware, unchanged thresholds, processing-only control, uncertainty and complete declared workload coverage | Exploratory v1 evidence exists. v4 adds near-limit bodies and descriptor recovery; a whole ordinary HTTP connection probe now has matched immediate-I/O and held-writer/loopback controls. Numerical qualification, missing metrics/workloads and representative hardware remain open. |
| Real evaluator and task evidence | Explicit matched trials with fixed fixtures/rubrics/models and separately reported availability, transport, policy and task quality | Compatible and actual Auto simple Jev preflights pass. Two five-pair real Tev1 experiments retain identical warm classification failures, finite-deadline stress differences and matching zero-deadline outcomes. Full profile/task and larger-host evidence remain open. |
| Rust contributor/release tools | Original tool behavior mappings, release state handling, immutable archives, registry verification, direct installation and source/build provenance | Most tooling exists; original tooling assertions and production release workflow/provenance remain incomplete. |
| Native CI and supported platform matrix | Compiler/feature/license checks, installed native/npm/direct artifacts, minimum OS/libc/CPU/TLS/PTY/WSL evidence for every approved target | Ten upstream checks pass at the initial source. Full matrix is explicitly unapproved; additional architectures cannot be silently dropped. |
| Default cutover and retirement | All preceding gates approved, separately authorized release, migration/help/contributor docs, retained rollback and defined stabilization period | JavaScript remains shipping default. This phase remains required for full completion. |

The [assertion ledger](../rust/parity/coverage.json) and [evidence index](../rust/parity/evidence.json) retain individual test status. Existing report hashes refer to historical inputs; editing code or a manifest does not transfer old qualification to new bytes.

## Behavior and invariant audit

Each area must retain its full baseline behavior, not only its main happy path:

- Commands: default invocation, setup, configuration, history, doctor/local diagnosis, Claude, serve, help/version/JSON/exit codes, hidden renderer and opaque Claude arguments.
- Configuration: exact defaults and empty/null distinctions; XDG/home/explicit lexical paths; file/environment precedence; force/replace and malformed-file preservation; no implicit project environment loading.
- Policy: fixed trusted system files, owner/mode/type/symlink admission, fail-closed errors, allowlists/locks and repair paths without bypass.
- Secrets: hidden/stdin-only input, unchanged Keychain service/account digest, read-back and migration ordering, environment/locked-store behavior and no history access to Keychain.
- Launcher: unmodified external Claude, authenticated loopback gateway, terminal/signal/exit behavior, secret filtering, option precedence, permission-path rebasing and private cleanup after success/failure/crash.
- Gateway: endpoint/method/auth/beta/query/header matrix, bounded input, compression rejection, transparent compressed replies, malformed/framing/deadline/disconnect behavior.
- Classification: both native System One evaluators, unchanged inputs/rubrics/confidence floors, exact redaction/extraction/cache identities, shared work and independent cancellation.
- Ollama operations: experimental local default, native model metadata admission, no hidden hosted fallback, warmup/residency/keep-alive, zero-runtime-timeout cancellation, explicit pull and no-inference plain doctor.
- Compatibility: exact model facts, all compatible tiers, Auto Sonnet/Opus/Haiku floor, unknown extensions, signed thinking/safeguards, tool/system/attachment/context/count policy.
- Continuity: session/agent/prompt isolation and content aliases, goals/new tasks/tools, active versus retired records, capacity and supersession, selected versus cleanly confirmed execution.
- Status/telemetry: foreground selection, requested/selected/observed models, bounded independent storage, safe callbacks/normalizers, controls/width/color and no inference delay from observation failures.
- History/savings: logging off by default, prompt versus metadata privacy, bounded private queues/readers including FIFOs, schemas 1/2, explicit partial coverage and immutable historical pricing provenance.
- Tools: evaluation thresholds/held-out identity, opt-in isolated canaries/probes, runtime-neutral/storage benchmarks, transfer bundles, exact archive checks, registry verification and uncertain publication handling.

Representation and transport obligations cross these areas: JavaScript property order/duplicate keys/numbers; UTF-16, code-point and UTF-8 budgets; lone surrogates; deeply opaque extensions; URL/environment/date/path identities; response-head/body/final-flush ownership; cached counts and independent worker cancellation. Native API differences such as typed cancellation versus arbitrary JavaScript Error identity need explicit migration review and native tests, not fabricated identical inputs.

## Performance gates and workloads

Retain the [frozen gates](../rust/parity/performance-gates.json): startup ratio at most 0.5, idle RSS at most 0.6, peak RSS at most 0.7, CPU/request at most 0.7 and processing-only large-catalog p95 at most 0.5. No repeated p95/p99 or throughput regression greater than 10% outside declared uncertainty; zero unexplained functional/evaluator-policy mismatch. Do not change these thresholds after seeing candidate results.

Required measurement coverage includes:

- Cold/warm help, version and status; service readiness; direct mock transport control and immediate/fixed-delay evaluators.
- Small/large catalogs, near-limit/tiny-string opaque bodies, cache hits/misses and identical/distinct agent requests.
- Pinned/new/goal turns and count calls; concurrency 1/8/32/128; capacity exhaustion and cache isolation.
- Slow clients; fragmented/compressed SSE/JSON; 429/5xx/malformed/truncated replies; evaluator timeout and zero-timeout cancellation.
- Cancellation at every stage; slow/failed storage; status polling and history at read limits; repeated launch/kill and long-running mixed recovery.
- Startup/ready/routing/first-body distributions, stalls/throughput, process CPU, allocations, idle/true peak RSS, descriptors, call counts and complete cleanup.

Preserve all unsuccessful outcomes and immutable source/binary/fixture/protocol identities. The user confirmed that only normal-use conditions are available on this 16 GiB Mac; it was on battery during preparation. Record these observations as normal-use evidence and keep controlled representative-hardware gates pending. On 2026-10-14 the user deferred the larger-Mac local-model trial because that machine is unavailable. Keep that result unmeasured and its qualification pending; the portable comparison command remains available for future use. Model trials need digests, runtime, residency and cold/warm conditions; live trials use disposable synthetic projects and exclude unrelated startup/MCP customizations.

## Distribution and cutover audit

The [platform inventory](../rust/distribution/platforms.json) remains authoritative and unapproved. Investigate all baseline architectures, GNU/musl choices, minimum OS/libc/kernel and corporate/custom/system CA behavior. Current Claude architecture support does not erase AutoRouter serve/config/history use on other Node-supported platforms.

Every final npm/direct artifact needs exact installed bytes/modes, complete licenses and reviewed source/build provenance, source ancestry, compiler/lock/features/CPU/linkage checks, same-prefix upgrade/rollback and failure cleanup. The entire approved binary matrix must fit 32 MiB compressed / 64 MiB expanded / 32 MiB per file / 256 entries. A host archive does not prove aggregate fit. Optional dependency packages or broader archive bounds require an explicitly reviewed contract change, not silent adoption.

Update release verification and publishing around the exact qualified native set before retiring source-file publication checks. Preserve branch/tag protections and four Node/OS jobs; at cutover those jobs still verify npm installation and native execution. Retain the frozen oracle for a defined stabilization period, direct/native rollback artifacts, ESM migration documentation and old history/price readers. Publication and a switch of the shipping default require final review of concrete qualified artifacts.

## Current implementation slices

1. Harden the scheduling-sensitive subprocess test while preserving its production deadline and privacy assertions.
2. Execute all original Auto/goal HTTP schedules and map every variant/assertion to actual native behavior.
3. Add exact direct-archive installed lifecycle execution, keeping private feasibility separate from production authorization.
4. Extend the frozen benchmark protocols to missing near-limit, opaque-shape and descriptor-recovery measurements; retain all prior results and gate limitations.
5. Continue with the remaining original server/CLI/tool assertions, typed-contract review, workload/metric implementations and platform/canary qualification. None of the preceding slices replaces the remaining plan.

## Recorded local evaluator investigation

The [real Tev1 summary](../rust/parity/measurements/resume-local-model-tev1-summary.json) retains two separately declared five-pair comparisons on the 16 GiB M4 under normal use. Both use the same `tev1:0.8b` digest, Ollama 0.40.0, frozen fixture/rubric and immutable native debug tool; builds/tests were paused during calls. All 450 held-out requests per engine have identical categorical results, excerpt byte counts and provider token metrics. Agreement is 73.3%, under-routing is 20%, and the unchanged strict quality gate fails in both engines. All three tiers occur.

With the 1500 ms deadline, full-budget stress has 10 bidirectional availability differences across pairs; Node records 16 timeouts and Rust 22. Those results remain retained and unqualified. The separate zero-deadline experiment matches all cold/warm/stress categorical fields and provider token metrics, with all 40 stress calls per engine succeeding and tested-model unload verified. It exercises the existing timer-disable option without changing labels or thresholds. Normal-use timing variations do not establish their cause, a Rust speedup, controlled hardware acceptance or Claude task completion. The larger-host trial and full rewrite gates remain required.

## Bounds corrections after the combined checkpoint

The [combined checkpoint](../rust/parity/measurements/resume-union-checkpoint-summary.json) passed formatting, strict workspace Clippy and 1,120 Rust tests before the following corrections. Its source and logs remain retained; it does not qualify later edits.

The public history reader previously accepted directly constructed zero or raised limits. It now validates all seven fields before worker admission, ID validation or filesystem access. [Focused evidence](../rust/parity/measurements/native-history-limit-admission-summary.json) records 17 passing native tests and frozen Node rejection controls with zero history filesystem calls. The default ceilings and valid smaller limits are unchanged.

Status cleanup previously rejected safe-integer PIDs above `u32::MAX`, which could preserve a fresh directory that the source removed. [The correction](../rust/parity/measurements/native-safe-pid-cleanup-summary.json) preserves the exact integer for the liveness callback; all 34 source/native file cases match and seven focused tests pass. The default callback rejects values outside the positive signed operating-system PID range before any signal query. The callback's `u32` to `u64` migration is documented in [the embedding guide](../rust/docs/embedding.md).

These focused results do not replace final combined tests, exact archive qualification, remaining timestamp and reader boundaries, or platform validation.

The additional [failed-probe regression](../rust/parity/measurements/native-cleanup-probe-summary.json) fixes per-directory callback containment: uncertain liveness skips removal and allows later directories to be processed. Its isolated child test confirms private panic-output suppression after explicit initialization and delegation of unrelated panics to the host hook.

The [Keychain subprocess correction](../rust/parity/measurements/native-keychain-process-bounds-summary.json) restores the source's shared 64 KiB stdout/stderr ceiling. Actual synthetic children prove the exact combined boundary, stdin closure, production 15-second timer under a paused clock, and direct-child reaping before awaited failures return. Future drop remains nonblocking with eventual cleanup on a live runtime. These tests perform no real Keychain operations.

The [committed host archive checkpoint](../rust/parity/measurements/committed-bdf6af1-host-archives-summary.json) binds the ordinary CLI from clean commit `bdf6af1` to its exact npm and direct archives. Each installed archive passes 74 launcher tests across seven suites; the npm path includes offline installation with scripts disabled, and both execute without Node in their runtime path. Independent Cargo feature inspection confirms development probes and transport experiments are absent. These are current-host feasibility checks, with full platform qualification, final license review and shipping provenance still pending. Later changes need their own qualification.

The [cache boundary comparison](../rust/parity/measurements/native-cache-boundaries-summary.json) exercises all 1,000 classifier entries and 100 token-count entries, touched-entry eviction, exact 300,000 ms expiry and zero embedding options. Nine frozen source controls and five native tests pass; independent review compares all 2,126 mock wire calls and 3,248 ordered steps, including actual confirmed task ownership through eviction and expiry. The native private cache size is not fabricated, and raw timing remains separate. This supplies boundary evidence without changing the original-definition coverage count or approving the broader limit audit.

The [original compatibility schedules](../rust/parity/assertion-mappings/routing-compatibility-contracts.json) now replay all 73 assertions in definitions 2–5, including forced tools across all three profiles, five malformed shapes at both endpoints, opaque same-model requests and incompatible count targets. The [permission-overlay schedules](../rust/parity/assertion-mappings/status-settings-file-contracts.json) exercise definitions 4 and 6 with actual relative source files, complete allow/ask/deny anchors, literal glob escaping and unchanged source bytes. Explicit native format and callback ownership differences remain recorded.

The [prompt-order regression](../rust/parity/assertion-mappings/hidden-prompt-order-contracts.json) checks actual PTY attributes inside the invitation writer, captures real output, and verifies exact secret return and restoration. A separate invitation-panic control restores all terminal attributes and public file flags. The retained Darwin write-marker diagnostic was a test expectation issue, reproduced without Tokio; it did not change secret-input behavior.

The [savings ingress review](../rust/parity/measurements/native-savings-ingress-summary.json) compares all 163 ordered snapshots and 46 original predicates, including every invalid usage example and the complete dated six-model pricing table. The [setup environment review](../rust/parity/measurements/native-onboarding-environment-summary.json) binds four source/native JSON rejection messages and twelve real compiler controls to a freshly passing 35-scenario native CLI matrix. Two older focused logs are unavailable locally; their original identities remain recorded separately. Each accepted language migration is explicit: native owned JSON excludes executable getters and nonfinite scalar values, pricing facts are detached copies, and setup accepts OS strings. These reviews do not fabricate unsupported calls or approve the remaining definitions.

The [portable local evaluator tool](../rust/docs/local-model-comparison.md) now separates metadata-only preparation from explicitly requested paired inference. [Mock validation](../rust/parity/measurements/native-portable-local-comparison-summary.json) passes 11 schema controls, seven process controls, a complete five-pair CLI run with 990 synthetic calls, and interrupted-run cleanup. Model/runtime/source drift and existing output paths are rejected. No real model is measured by these tests, and the larger-Mac trial remains deferred. Child output, retained evidence and daemon cleanup have separately stated bounds and limits.

The [combined input and tooling checkpoint](../rust/parity/measurements/native-migration-checkpoint-20261014-summary.json) passes formatting, strict workspace Clippy and 1,183 Rust tests under default parallelism, with zero failures and seven default-ignored entries. Separate synthetic orchestration checks exercise the two ignored portable comparison integrations. Current Node contracts, all 596 tests and the installed 58-file shipping package also pass. The fresh source/reference transfer occupies 33,367,040 expanded bytes within the unchanged 32 MiB limit, leaving 187,392 bytes. These source-bound checks do not qualify a new ordinary native binary, numerical performance, real Claude task quality, the full platform matrix or default cutover.
