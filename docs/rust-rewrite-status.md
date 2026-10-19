# Rust rewrite completion audit

This checklist tracks the full [rewrite plan](rust-rewrite-plan.md), including its cutover phase. It does not redefine completion as an experimental candidate, four convenient platforms, or a passing subset of tests. The frozen baseline is `ea930c247626ce2af5ccdad721b5121417bf4ad8` / JavaScript 0.5.2. Status reviewed on 2026-10-19; changes in progress require fresh validation before approval.

## Completion criteria and authoritative evidence

| Plan requirement | Evidence required | Current state / next work |
| --- | --- | --- |
| One native executable, every product command, no JavaScript sidecar | Installed command/JSON/exit-code matrix, Node-free execution and binary/build feature inspection | Implemented experimental candidate; host tests pass. Complete supported-platform execution remains open. |
| Keep package/command names, npm global installation and direct archives | Exact npm and per-target archive identities, scripts-disabled offline installation, installed byte/mode checks and lifecycle execution | The [current exact host archives](../rust/parity/measurements/native-owned-npm-smoke-identity-summary.json) each pass 92 installed lifecycle checks, with complete byte/mode and launcher validation before execution. These remain private feasibility artifacts with local prebuilt provenance. Source-to-binary reproduction, other targets and the production release set remain open. |
| Retain all thirteen behavior areas | Individual original-assertion mappings, shared fixtures, Rust tests and explained interface differences | Coverage ledger: all 588 original definitions now have individual mappings and explicit acceptance, with no partial or pending definitions. The [same-artifact benchmark-bundle review](../rust/parity/assertion-mappings/benchmark-bundle-same-artifact-native-api-migration.json) closes the last definition using actual complete sources and runnable evaluator help in one archive, with three explicit native format/API migrations. Earlier readiness, package diagnostics, evaluation policy and publication preflight migrations remain individually recorded. All 51 original Router definitions have individual mappings. All thirteen areas still require their broader qualification; fixture counts do not establish whole-area approval. |
| Transparent provider responses, signed history, safety and permissions | Actual HTTP/SSE/JSON/compression/framing comparisons, client disconnects, failed flushes and pipelined-response ownership | Substantial synthetic coverage; unresolved transport/TLS differences and unreviewed original HTTP schedules remain. |
| Authentication, refreshed subscription credentials and custom-upstream restrictions | Request-scoped credential tests, API/OAuth/beta rejection matrix, conservative unsupported-contract behavior | Implemented and tested on synthetic transports; live canaries and supported-platform TLS qualification remain. |
| Configuration, policy, Keychain and history without mandatory migration | Exact lexical identities, permissions/revision/fsync/no-follow checks, policy admission, isolated migration/rollback and existing schemas/prices | Isolated real macOS Keychain migration passes all 25 host assertions through both engines. Complete assertion review, failure/rollback and platform qualification remain. |
| Bounds, cancellation and state ownership | Full boundary/interleaving matrix; bounded retention, no uncancelled workers, clean shutdown, no false continuation confirmation | All 53 fixed-limit rows and 27 configuration keys retain individual acceptance. The [approved schema 7 checkpoint](../rust/parity/measurements/native-schema7-joint-budget-summary.json) passes the full correctness workspace: 1,548 passed, zero failed and eleven ignored across 66 groups. The default developer transfer now has separately enforced 40 MiB joint carrier/logical limits; every source archive retains its 32 MiB limit and legacy schemas retain joint 32 MiB. Complete CLI export/restore verifies all 709 raw source inputs and 113 restored reference files. Three new regressions cover budgets, forgery and complete-source restoration. Shipping native-package limits are unchanged. Broader protocol/resource/platform qualification remains open. |
| Predeclared speed and memory gates | Fresh matched five-round reports on representative hardware, unchanged thresholds, processing-only control, uncertainty and complete declared workload coverage | The [exact-capacity comparison](../rust/parity/measurements/native-exact-json-capacity-memory-summary.json) preserves request bytes and matches all 60 large-catalog rows. Its unchanged 0.7 memory target still fails at 128 clients: ratio 0.865, interval 0.849–0.906. The [earlier 0.915 failure](../rust/parity/measurements/native-large-catalog-lifetime-memory-summary.json) remains retained. Its 1/8/32 memory, CPU, idle RSS and throughput targets are observed on this normal-use 16 GiB host. The separate [streamed-classifier result](../rust/parity/measurements/native-processing-hash-stream-summary.json) retains its observed 0.442 processing-p95 ratio and 0.438–0.444 interval. High-concurrency memory, representative hardware, missing workloads/metrics and shipping-artifact qualification keep the full gate open. |
| Real evaluator and task evidence | Explicit matched trials with fixed fixtures/rubrics/models and separately reported availability, transport, policy and task quality | Compatible and actual Auto simple Jev preflights pass. Two five-pair real Tev1 experiments retain identical warm classification failures, finite-deadline stress differences and matching zero-deadline outcomes. Full profile/task and larger-host evidence remain open. |
| Rust contributor/release tools | Original tool behavior mappings, release state handling, immutable archives, registry verification, direct installation and source/build provenance | Most tooling exists; production release workflow/provenance and broader tooling qualification remain incomplete. |
| Native CI and supported platform matrix | Compiler/feature/license checks, installed native/npm/direct artifacts, minimum OS/libc/CPU/TLS/PTY/WSL evidence for every approved target | Fresh remote CI is required for the approved schema 7 change; its [local validation](../rust/parity/measurements/native-schema7-joint-budget-summary.json) passes. The [terminal `475454a` CI audit](../rust/parity/measurements/native-475454a-ci-summary.json) remains historical: four Node jobs, core32 and runtime32 succeeded, while all eight host/GNU/musl jobs failed only the then-unchanged joint 32 MiB transfer test. Those failures remain recorded; later independent stages succeeded and both musl SDKs verified all 61 pinned APKs. Artifact metadata is separate from downloaded payload proof. The [earlier `9448681` i686 proof](../rust/parity/measurements/native-9448681-ci-summary.json) also remains historical. Complete platform, minimum-OS, TLS, WSL, provenance and release qualification remain open. |
| Default cutover and retirement | All preceding gates approved, separately authorized release, migration/help/contributor docs, retained rollback and defined stabilization period | JavaScript remains shipping default. This phase remains required for full completion. |

The [assertion ledger](../rust/parity/coverage.json) and [evidence index](../rust/parity/evidence.json) retain individual test status. Existing report hashes refer to historical inputs; editing code or a manifest does not transfer old qualification to new bytes.

The [direct-install rehearsals](../rust/parity/measurements/native-direct-recovery-and-compact-source-summary.json) pass three same-prefix physical upgrade/rollback schedules with two retained private 0.5.2 artifacts, including failed-startup recovery and unchanged configuration/history; the legacy npm rehearsal also passes. These are normal-use Mac ARM observations with explicitly unverified build provenance. The new source-only `.cpio.gz` archive independently restores all 709 members, including 707 raw inputs and the unchanged generated README and ordered schema 2 JSON. Its expanded carrier is 33,102,336 bytes, leaving 452,096 bytes under the unchanged limit. Explicit legacy tar readers and native/npm/direct archive policies remain intact; full-current-tree USTAR export honestly refuses its expansion limit. This supporting API migration changes no original-definition, limit or key acceptance count and does not resolve the separate joint source/reference limit.

The [eight-client delivery tests](../rust/parity/measurements/native-eight-gateway-full-block-owners-summary.json) use one Gateway and shared experimental buffered transport over synthetic HTTP and pinned upstream TLS 1.2/1.3. A partial-provider-write gate establishes eight live sockets; injected downstream Pending writes exhaust twelve retained block credits per pool. They verify isolated cancellation and expiry, six complete responses, a ninth physical recovery, exact tool ownership and zero-resource cleanup. Earlier queue-saturation and fixture-oracle failures remain recorded; this distinct full-credit schedule does not turn them into passes. The production executable remains byte-identical, and these tests do not qualify real providers, OS-only backpressure, performance or shipping TLS.

The [exact JSON allocation checkpoint](../rust/parity/measurements/native-exact-json-capacity-memory-summary.json) uses the unchanged writer and one exactly sized count buffer. Complete synthetic wire bytes match the retained original libraries; three new regressions cover sizing/memo isolation, deep Unicode/number serialization and projected field order/adaptation. Combined same-source Rust checks retain every previous named outcome and add three passes: 1,544 passed, one unchanged joint 32 MiB transfer failure and eleven ignored entries. The workspace stopped at that failure; the missing integration tail and doctests completed separately. Formatting, strict Clippy, all 645 Node tests, the 58-file/17-scenario package check and three required same-artifact bundle integrations pass. The source-only container restores all 710 members under unchanged limits. The measured release executable is newly compiled over parent `52b79e8`; broader installed native, CI, TLS, performance and cutover qualification remains open.

The [schema 7 developer-transfer checkpoint](../rust/parity/measurements/native-schema7-joint-budget-summary.json) applies the separately approved 40 MiB joint budgets. Its complete transfer is 34,421,248 expanded carrier bytes and 34,445,254 logical bytes, including every logical copy of shared files. The source archive uses 33,085,952 expanded bytes, retaining 468,480 bytes of headroom under its unchanged 32 MiB cap. Default and explicit schema 7 packets are byte-identical; explicit schema 6 still refuses this oversized payload before creating a destination. Complete source bytes, modes and indexed digests, all 113 restored reference files, unknown-version rejection and forged-limit controls pass. All 1,548 Rust tests, formatting, strict Clippy, 645 Node tests, the 58-file/17-scenario package check and three optimized bundle integrations pass. The first cold optimized workspace compilation was stopped before tests; the full workspace result uses the CI correctness profile. The production binary remains byte-identical to the prior allocation candidate. Remote CI and broader rewrite qualification remain pending.

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

1. Finish the additive exact-archive upgrade/rollback CI checks on all current native packaging environments, retaining the successful and failed evidence separately.
2. Extend execution coverage to remaining baseline architectures with pinned toolchains and real target userlands; emulated execution must remain separate from CPU/minimum-kernel qualification.
3. Complete broader protocol/resource qualification. All 588 original definitions, 53 fixed-limit rows and 27 configuration keys now have individual acceptance; this does not qualify the thirteen behavior areas or their integration gates.
4. Complete TLS/protocol/resource schedules and the remaining failed-startup, partial-write and ownership scenarios for rollback.
5. Meet every unchanged numerical gate and declared workload/hardware requirement, complete real-task evidence and final source/build/license/installed-artifact qualification, then perform separately authorized stabilization and cutover.

The recorded fourteen green jobs at `7b419cb` establish only their recorded scopes. Later local changes require fresh CI, and none of these slices replaces the full plan.

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

The subsequent [clean `1fec549` host archive check](../rust/parity/measurements/committed-1fec549-host-archives-summary.json) builds the ordinary CLI and development helper separately and confirms that experimental transport features and development probes are absent. Both exact native npm and direct archives pass all 74 installed launcher tests. This qualifies the recorded host artifacts under synthetic services; minimum macOS execution, the full target set, license review, shipping provenance and release approval remain open. Later source changes retain separate qualification.

The [per-definition CLI binding review](../rust/parity/measurements/native-1fec549-cli-definition-bindings-summary.json) separately binds all 59 native anchors across 54 existing mappings to actual pass lines and committed `1fec549` source. Exact historical function/assertion comparisons are retained where available; missing older logs and installed artifacts are not reconstructed. This refreshes evidence without promoting partial definitions or qualifying later source.

The [extreme cache lifetime correction](../rust/parity/measurements/native-cache-ttl-correction-summary.json) fixes a directly observed token-counter overflow and poisoned cache after a successful response. Checked expiry preserves successful caching and normal LRU/capacity eviction when the requested lifetime exceeds the clock's range. Eleven new regressions, existing focused tests and exact overlapping provider-byte comparisons pass. Defaults, deadlines and cancellation remain unchanged. This production change requires new combined and installed-archive checks.

The [original statusline CLI matrix](../rust/parity/measurements/native-statusline-cli-summary.json) now exercises all eleven original predicates across seven executable branches, including positive ANSI color, `NO_COLOR`, `TERM=dumb`, private metadata, malformed/oversized input and oversized/missing files. The original Node callback and native test both pass. The initial non-executable frozen-reference mode is retained as a failed setup attempt; the retry uses current executable-mode files verified byte-identical to the frozen source. No oracle bytes or modes were modified.

The [doctor dispatcher review](../rust/parity/measurements/native-doctor-dispatch-summary.json) adds 15 actual CLI scenarios for local diagnosis without cloud credentials or Claude authentication, full singular JSON, cloud-backend rejection and cancellation during headers/body reads. Three focused tests and all 27 onboarding tests pass. All 168 synthetic requests and eight stalled-connection closures are retained. Original arbitrary injected-report and caller-error identity have explicit native API migrations; no equal object calls are invented. New installed-archive execution remains separate. The user-requested debug-cache cleanup preserves reports, source and compiler fingerprint metadata while removing rebuildable binaries and libraries.

## Paired live-validation tooling checkpoint

The [focused tooling evidence](../rust/parity/measurements/native-paired-live-tooling-focused-summary.json) records seven schema controls, twenty owned report-stub commands, sixteen actual native route-metadata rows over scripted transport, and a one-admission TERM cancellation control. Formatting, strict xtask Clippy, help dispatch and the temporary Node adapter syntax also pass. These are separate tool checks; no real provider calls, full live reports or completed tasks were executed. The earlier failed test-style check remains retained.

The [paired live command](../rust/docs/compare-live.md) declares 120 fixed cases in twenty alternating harness invocations, preserving the existing profile rubrics and relative gates. Full current-source validation, the unchanged source/reference transfer cap, a matching helper build and actual real-provider qualification remain pending.

## Prompt excerpt and combined source checkpoint

The [prompt-excerpt correction](../rust/parity/measurements/native-prompt-excerpt-admission-summary.json) closes the directly reproduced unsafe-public-limit and eager-text-collection gaps. Twenty-six focused tests and sixteen actual compiler consumers pass. The review records parsed-input, primitive-parameter and checked-result API migrations per assertion; live JavaScript getters and arbitrary errors are not replayed as fake native values. The Value convenience facade still serializes its full input before document collection.

The current production-source checkpoint passed 1,209 Rust tests with nine default-ignored cases, formatting and strict workspace Clippy. The shipping JavaScript static checks, all 596 tests and its exact 58-file installed package also pass. The separately admitted onboarding test and bounded-reader ordering work require their own validation; these results do not qualify later changes or the final native distribution.

The [onboarding cancellation review](../rust/parity/assertion-mappings/onboarding-after-prompt-contracts.json) executes the previously missing cancellation immediately after a successful API-key prompt. Its new test covers valid, empty and multiline returned keys; all three stop before validation, local requests, Keychain work or saving. The full eight-test module and strict CLI checks pass. Production already had the checkpoint, so only the regression changed.

The [bounded-reader ordering correction](../rust/parity/measurements/native-bounded-reader-order-correction-summary.json) rejects invalid public limits before cancellation, preserving cancellation before oversized headers for valid limits. Actual source controls and all three native readers expose eighteen pre-fix ordering differences and none after the narrow guard change. All 72 native cases release their concrete test bodies; six integration tests, nine reader units and strict runtime checks pass. Arbitrary caller error identity, Web stream cancellation and native ownership remain separate embedding contracts, so the original partial definitions are not promoted by this correction.

The [final source checkpoint](../rust/parity/measurements/native-final-1211-checkpoint-summary.json) includes the onboarding regression and reader correction: formatting, strict workspace Clippy and all 1,211 default Rust tests pass, with nine ignored entries and zero failures. Both explicit paired-live synthetic controls also pass after factoring. The matching development helper creates the complete source/reference transfer at 33,552,896 expanded bytes within the unchanged 32 MiB limit, leaving 1,536 bytes. Exact logs and 367 source/static-include inputs are retained. Current JavaScript checks, all 596 tests and its installed package remain separately recorded; fresh native archives, real tasks, performance and platform qualification still require their own checks.

The [clean `0af8f08` host archives](../rust/parity/measurements/committed-0af8f08-host-archives-summary.json) build the ordinary CLI and development helper separately. Independent feature inspection passes, and each exact npm/direct archive passes all 78 installed tests across seven suites. Offline npm installation keeps scripts disabled and runtime execution needs no Node. This is current-host synthetic evidence; minimum environments, the full target set and final release provenance remain pending.

The [first five-pair whole-processing measurement](../rust/parity/measurements/native-0af8f08-whole-processing-summary.json) retains 200 warmup and 2,000 measured samples per engine per round on the normal-use 16 GiB M4. Every request's byte, policy, parsed count and delivery evidence matches, including fresh ordinary-loopback controls. The median native/reference p95 ratio is 0.524 with a 95% interval of 0.502–0.542, so the unchanged maximum of 0.5 is not met. Independent review recomputes all distributions and the original 5,000-resample interval. Existing swap pressure and raw power/memory conditions are retained; representative-hardware, shipping-artifact and other workload/resource acceptance remain unqualified. This failure motivates processing optimization rather than a threshold change.

## Context processing and musl CI checkpoint

The [context byte-charge change](../rust/parity/measurements/native-context-byte-charge-summary.json) uses the existing exact serialization counter instead of constructing the entire normalized request string, and replaces only the deferred tools value charge algebraically. Actual frozen Node controls retain Unicode/number/property normalization and all three 150,000-byte boundary cases. Original context and runtime routing replays pass, followed by formatting, strict workspace Clippy and 1,212 passing Rust tests with nine ignored entries and zero failures. The matching helper regenerates the source/reference transfer at exactly 33,554,432 expanded bytes, the unchanged 32 MiB limit. Additional required work needs legitimate factoring and fresh transfer validation; inputs and bounds cannot be silently reduced.

The [second five-pair whole-processing measurement](../rust/parity/measurements/native-context-processing-whole-summary.json) uses that exact helper and the unchanged protocol. All 22,000 proof rows and fresh loopback controls match, and independent review recomputes all 20,000 measured samples and the original bootstrap. The observed p95 ratio is 0.641 with a 95% interval of 0.568–1.122, so the target is still not met. Usual-app normal-use conditions, changed load/memory snapshots and the preceding user-requested compiler-cache cleanup are recorded; the result does not establish a causal improvement or regression from the source change. Both failed experiments remain retained, with no repeated run to seek a passing result.

An independent review also exposes a pre-existing 32-bit deferred-expansion arithmetic gap. Frozen Node produces 4,295,090,321 and 5,000,105,126-byte charges from two synthetic requests below 2 MiB. The current native `usize` arithmetic cannot represent those values on armv7 and can wrap below the routing guard. These are actual Source results and native arithmetic analysis, not executed 32-bit proof. A Source-compatible representation, explicit native API migration and real 32-bit validation remain required; a saturated count is not accepted as exact telemetry.

The [additive musl CI preparation](../rust/parity/measurements/native-musl-ci-admission-summary.json) adds native x86_64/ARM64 Alpine builds, independent same-byte rebuild checks, static linkage inspection, synthetic transport/TLS/localhost controls, exact installed archives and independently observed mixed musl/GNU dispatcher selection. Existing workflows and required Node checks remain unchanged. Syntax, bounded process-admission controls and peer review qualify the tooling preparation only. Actual CI execution and every supported-platform minimum environment remain pending; this does not approve the complete target matrix or a release.

The [first actual musl SDK execution](../rust/parity/measurements/native-musl-sdk-correction-summary.json) fails both setup jobs before compilation or product tests. x86_64 passes its signed-index hash but rejects eleven version-qualified arguments as literal package names; ARM rejects a changed signed-index hash. The reviewed correction fetches actual package names while asserting all eleven unchanged installed versions, and refreshes only the independently verified ARM index pin. Signature/hash checks and byte provenance remain enforced. Failed inputs and source diagnosis are retained separately; the corrected workflow still needs actual execution and cannot establish platform qualification from syntax checks.

The [second musl SDK attempt](../rust/parity/measurements/native-musl-tar-correction-summary.json) passes the fixed index checks, dependency installation, all eleven version guards and both official Rust archive hashes. Both jobs then fail during archive extraction before product compilation. The next reviewed correction pins build-only GNU tar and explicitly uses the already-pinned full XZ program, preserving the same archive/index/image identities and existing package versions. Builder provenance schema 2 adds actual tar/XZ versions. The exact failed decoder mechanism remains unproven; successful extraction and product qualification require fresh CI execution.

The [committed `2887f2c` CI checkpoint](../rust/parity/measurements/committed-2887f2c-ci-summary.json) passes all ten existing Node/native/GNU jobs, including all four required Node/OS jobs. Both additive musl jobs fail in SDK extraction before product compilation or tests. This approves neither the complete twelve-job matrix nor later source changes, minimum platforms, performance, release or cutover.

## Context arithmetic, document memo and compiler-context hardening

The [current source hardening checkpoint](../rust/parity/measurements/native-context-hardening-checkpoint-summary.json) uses Source Number arithmetic for deferred expansion, with explicit typed unavailability and conservative capacity checks. It adds successful-only document length memoization with current-limit checks, independent clones and mutation invalidation; model-dependent charges remain separate. Original source controls, numeric order and all API consumers are reviewed. No routing policy, thresholds, deadlines or cancellation bounds change.

The checkpoint also repairs standalone compiler-control dependency selection: the exact executed-test fingerprint replaces latest-file selection, and explicit target builds include host proc-macro dependencies and tokenized flags. All thirty original compiler consumers remain intact. The three suites pass all 21 tests in an actual explicit native Mac target layout. Actual musl and 32-bit execution remain pending. The additive i686 CI preparation requires every core test binary to be ELF32, executes the full unfiltered core package and verifies arithmetic/compiler callbacks; syntax and peer review are preparation evidence only.

After factoring unchanged shared test support, formatting, strict workspace/all-target Clippy and all 1,228 default Rust tests pass, with zero failures and nine ignored entries across 63 groups. The earlier full run failed the unchanged 32 MiB transfer gate by 7,680 bytes; the same gate now passes without omitting inputs or raising the cap. A matching retained helper transfer, fresh ordinary CLI/installed archives, numerical performance, real tasks and complete platform/release qualification remain separate work. Original coverage stays 521 covered/67 partial.

## Owned diagnostic progress futures

The [per-assertion supplement](../rust/parity/assertion-mappings/diagnostic-progress-future-contracts.json) completes only original diagnostic definition #14. Its sole assertion executes for both throwing and asynchronous rejecting callbacks. The actual eight-test native run preserves the complete report for ordinary unwind and for fifteen returned, polled and released rejecting futures; the observer owner also joins cleanly. The [embedding guide](../rust/docs/embedding.md) explains the owned Rust adapter and its explicit API migration. The old partial mapping remains historical evidence. Coverage is 522 covered/66 partial; no whole area, platform, performance or cutover gate is promoted.

## Retained cffec checkpoint and current integration

The [reviewed checkpoint summary](../rust/parity/measurements/committed-cffec6c-qualification-summary.json) binds the clean local `cffec6c` artifacts and actual CI checkout `769f4a86` to the same Git tree. Ten main/GNU jobs pass; i686 core arithmetic passes 207 tests across twelve ELF32 groups. The latter is not runtime or installed i686 qualification. Both musl jobs fail the two terminal-restoration tests; the reviewed Nix initialization correction still needs fresh musl execution.

Each retained host npm/direct archive passes 78 installed checks. The third whole-connection measurement retains 22,000 matching proof rows and reports a median p95 ratio of 0.503931 with interval [0.488689, 0.638990]. Its 0.5 target remains inconclusive and unqualified. The attempted paired live trial stops on the first Node report identity rejection, admits no native pair and remains incomplete. The later numeric identity correction requires a fresh trial. All earlier outcomes stay retained.

The newer integration passes strict workspace checking and 1,233 executed tests, with eight ignored entries, before the added warning-future regression. Its one failing test is the combined source/reference transfer budget; later target groups are not thereby qualified. The independent bootstrap/source auditor passes 28 tests and verifies the full patched supplier while keeping its own registry dependency separate. Shared test helpers preserve the original named bodies and transport/ownership policies. Full workspace success, fresh ordinary artifacts, transfer fit, platform and performance qualification remain open.

The [session-warning future supplement](../rust/parity/measurements/native-original-session-warning-future-summary.json) covers only original session-log definition #12. All three Source assertions execute with the ordinary throwing callback and the actual rejected Promise; the new Rust replay uses the documented synchronous bridge and public owned-future adapter, polls and drops its rejecting future, and joins the observer owner. The focused module passes all 19 tests, including its 18 unchanged tests, and strict runtime/all-target Clippy passes. Promise/Error object identities and exact scheduling are explicit API differences. Historical partial evidence remains; the test executable and Cargo fingerprints were retained before compiler-cache cleanup. This focused evidence does not qualify the fresh workspace, source-transfer capacity, platforms, performance or release.

The [response-execution future supplement](../rust/parity/measurements/native-original-response-execution-future-summary.json) covers only original response-observer definition #32. Its inherited byte assertion executes twice, with all six original model/execution/completion callback arguments retained. The native replay forwards both original frames before polling its three returned futures, observes three rejections and drops, and joins the zero-charge observer owner. All 22 module tests pass, including all 21 unchanged tests; strict workspace/all-target Clippy passes after explicit transfer-map type annotations. The documented owned-future bridge replaces Promise/Error identity and scheduling. Clean observer EOF does not prove final downstream flushing or confirmed task continuity. Historical partial evidence and failed checks remain retained; full workspace/source-transfer/platform/performance/release gates need separate qualification.

## Retained version2 paired-transfer workspace checkpoint

The [retained version2 full workspace checkpoint](../rust/parity/measurements/native-transfer-v2-workspace-summary.json) passes 1,254 tests across 63 groups, with nine ignored tests and one source-transfer capacity failure. Strict workspace/all-target Clippy passes. The initial sandbox run remains retained: all 229 non-capacity failures recover with authorized localhost and owned-child access and unchanged implementation, tests, Cargo and transferred inputs. Four intervening history-label metadata corrections are recorded separately; the broad snapshots are not byte-identical. The same combined 32 MiB transfer limit still fails. A lossless CPIO representation has passed preliminary exact-byte/mode restoration for all 682 current source files, but its native codec, final-source fit and hostile-input controls still need execution. Neither this checkpoint nor the preliminary carrier experiment approves later source, platforms, performance or cutover.

## Returned JSON usage callback regression

The [response-usage future supplement](../rust/parity/measurements/native-original-response-usage-future-summary.json) completes only original response-observer definition #10. Its three direct assertions and four inherited byte checks execute across all original JSON schedules, including the real rejected Promise and the public native owned-future bridge. The focused module passes all 23 tests, preserving all 22 previous tests; strict workspace/all-target Clippy passes. Callback metadata stays separate from the complete synthetic provider body. Error/Promise identities and scheduling remain documented API differences; observer EOF does not establish downstream delivery or task continuity. Coverage is 525 covered/63 partial. Archive, full-workspace, platform, performance and release approval remain separate.

## Native lossless source handoff and serialized host checkpoint

The [version3 checkpoint](../rust/parity/measurements/native-newc-source-transfer-summary.json) restores all 684 native source members and 113 complete reference members exactly. Its strict newc native carrier and independently licensed tar delta retain every original input, mode and both separate 32 MiB limits. Stored expansion is 33,230,848 bytes and complete logical payload is 33,098,782 bytes, leaving 323,584 and 455,650 bytes of headroom. No file is removed or minified. The default source-only tar stays unchanged; historical schema1 and an explicitly derived full-historical-input schema2 receiver control pass. Both BSD readers restore every native byte/mode; they share libarchive, while the separate bounded parser compares entries independently. Linux and minimum-platform bootstrap remain unqualified.

All 1,265 tests pass across 63 groups in the serialized full workspace run, with nine opt-in tests ignored. All 1,299 git-listed repository inputs stay unchanged during that run. The previous default-parallel run remains failed: 1,260 pass and five process predicates fail. Their exact cause remains unproven; the heavy normal-use memory/load snapshots, unchanged production code and original deadlines are retained. Test-only failure diagnostics add safe status and owned-child state without changing predicates or deadlines. All 51 focused process controls pass serially. Internal test concurrency and numerical benchmark gates stay unchanged; serialized success does not approve default-parallel CI or performance. Required Node check, test and exact-package commands pass. Fresh supported-platform CI, ordinary shipping artifacts, outstanding parity, performance, live/hardware and cutover qualification remain open.

## Cleanup filesystem embedding boundary

The [per-assertion filesystem review](../rust/parity/measurements/native-original-status-cleanup-filesystem-summary.json) completes only original status-cleanup definition #5. Its exact frozen JavaScript test passes both assertions, with all 111 baseline files verified before and after. Current native missing-directory and private-child regressions pass; the non-root child exercises actual permission-denied metadata reads, disappearance, continued cleanup and recovery without exposing private errors. Rust uses concrete Tokio filesystem operations rather than JavaScript's injected filesystem object, as documented in the embedding guide. Coverage is 526 covered/62 partial. No product implementation changes or broader platform, CI, performance or cutover approval follow from this review.

## Historical `a6b49e2` native CI failure

The [13-job terminal observation](../rust/parity/measurements/committed-a6b49e2-ci-failure-summary.json) binds every job to the actual PR merge checkout and the pushed source tree. Five jobs pass: the four required Node jobs and the narrow 32-bit core job. Six ordinary/GNU jobs fail when concurrent builders construct the shared frozen reference; both musl jobs fail a default-stderr assertion in a synthetic launcher script. The four earlier terminal-restoration regressions pass on each musl target, but neither full musl job passes. These failed runs remain failed. The atomic reference publication and portable synthetic grep fixture are exercised in the later `83ae641` checkpoint below.

The atomic reference publication tests now pass with four concurrent builders and preexisting or raced valid, corrupt, empty, file and symlink destinations. The ten paired-transfer tests, independent auditor tests, strict Clippy and all three required Node checks pass. The full default-parallel Rust workspace passes 1,268 tests in 63 groups with nine opt-in tests ignored. The earlier failed default-parallel run remains failed. A fresh current-source transfer stays within both unchanged 32 MiB bounds: 33,243,648 expanded carrier bytes and 33,111,532 logical bytes. Its receiver restores all 113 reference files, including exact original hashes for all 111 frozen files and the license. The launcher fixture's five local logging tests pass. Fresh native CI, minimum platforms, performance and release qualification remain open.

## `83ae641` native CI checkpoint

The [next 13-job terminal observation](../rust/parity/measurements/committed-83ae641-ci-failure-summary.json) records ten successes and three failures for the same source tree as the PR merge checkout. All six ordinary Rust/GNU full-workspace runs pass the named frozen-reference controls, and both musl targets pass the launcher stderr regression. The Linux ARM configuration comparison stops at an unused-stdin `EPIPE` in its driver. Both musl targets reach the second-build byte comparison, where OpenSSL has embedded the different compiler-output directory names into `.rodata` and the build ID. The original failed jobs remain failed.

The configuration driver now writes stdin only for `--stdin` cases; its local frozen-reference run matches all 296 cases. The musl script now retains the first compiler-output tree and rebuilds from an empty tree at the same declared path, keeping the byte-for-byte comparison unchanged. These corrections require fresh ARM and musl CI before either is credited. At that checkpoint, 62 original definitions remained open; the complete platform matrix, numerical performance gates, release qualification and default cutover still remain open.

The [clean `5ad9a76` host archive checkpoint](../rust/parity/measurements/committed-5ad9a76-host-archives-summary.json) contains an exact npm archive and a direct macOS ARM archive. Each passes 78 installed launcher checks across seven suites, including an offline scripts-disabled npm installation and Node-free execution. The ordinary binary's production-feature audit passes. This is a current-host feasibility result: Cargo reused the unchanged ordinary binary, so source-to-binary reproduction, full license review and the platform/release gates remain unapproved.

The [policy fixed-path supplement](../rust/parity/measurements/native-original-policy-fixed-path-summary.json) completes only original policy definition #1. The exact frozen JavaScript test passes all three assertions. The matching native test passes on actual macOS and Linux CI hosts, exercising both compile-target path branches and the missing isolated-policy result. Its runtime-platform-string to compile-target-Path and `undefined` to `Option::None` changes are documented per assertion. At that checkpoint coverage was 527 covered/61 partial; no privileged system policy or broader platform approval follows.

## `5ad9a76` native CI checkpoint

The [terminal 13-job observation](../rust/parity/measurements/native-5ad9a76-ci-summary.json) records eleven successes and two musl failures for the pushed source tree and matching PR merge checkout. All four protected Node jobs, four ordinary Rust jobs, two GNU jobs and the 32-bit arithmetic job pass. Linux ARM completes the previously interrupted configuration comparison. Both musl targets produce matching independently rebuilt bytes at the same declared compiler-output path; this does not prove build-path independence.

The x86 musl job later times out in the first installed npm lifecycle check. Moving the compiler-output tree before that check removed its prebuilt debug test artifacts; cold compilation is a plausible explanation, not a proven cause because timed-out partial output was intentionally discarded. The ARM musl job passes its installed npm/direct checks and later fails the independent mixed-artifact dispatcher observation. Its old witness records a generic fixture failure, so the exact assertion and product-versus-harness cause remain unknown. The subsequent `ee4b60e` commit retains the initial test artifacts until the installed checks finish and adds bounded stage diagnostics to the dispatcher probe. Neither change qualifies either failed job until fresh CI executes those checks.

The [router count-start supplement](../rust/parity/measurements/native-original-router-count-start-summary.json) completes only original router definition #51. An admitted large-request count now starts before evaluator classification while their responses remain concurrent; tiny and hard-locked requests still skip counting. Three new native regressions cover start order, cancellation before the first count poll and shutdown before that poll. The full default-parallel workspace passes 1,274 tests with zero failures and nine opt-in tests ignored; strict workspace Clippy and the three required Node checks also pass. Coverage is 528 covered/60 partial. At advisory capacity the speculative count can still be omitted, and serializing its first poll ahead of Jev may affect latency. Numerical performance and current-source CI remain unqualified.

An ordinary-key object no longer allocates a position vector solely to preserve its existing insertion order; numeric-key objects keep the original sorting path. Frozen Node JSON cases, mutation cases and iterator differentials pass along with the full workspace. The change targets an observed processing cost, but no current-source benchmark demonstrates a speedup or satisfies the unchanged processing-only p95 gate.

## `ee4b60e` musl SDK index failure

The [new musl attempt](../rust/parity/measurements/native-ee4b60e-musl-index-drift-summary.json) fails both SDK jobs at the first pinned Alpine index checksum, before package fetching or Rust/product execution. Fresh bounded reads from the official repositories retain the new exact bytes and headers; both detached signatures verify against the named official Alpine public keys, and all twelve pinned root-package versions are unchanged. This supports signed upstream metadata drift. The failed CI cache bodies were not retained, so their exact identity with the later live reads is not claimed; signature verification also does not rule out upstream compromise.

Refreshing only the reviewed index pins and provenance preserves signature checks, both byte guards, exact installed-version assertions and offline installation. That correction needs fresh CI. The lifecycle and dispatcher changes were not reached by either failed SDK job, and the other workflows were not all terminal at this observation. No complete musl, platform, release or cutover qualification follows.

## Additive statusline startup benchmark

The [v5 benchmark protocol](../rust/parity/local-benchmark-v5.json) adds a private synthetic online status snapshot and stdin fixture at 40, 80, 100 and 120 columns without changing v1–v4. The [selected deterministic validation](../rust/parity/measurements/native-v5-status-startup-validation-summary.json) passes all eight frozen Node/native rows with exact declared output bytes; its report now limits parity scope to statusline process output, retains no timing samples and excludes every inherited HTTP and earlier startup workload. A first attempt with a 35 MB debug CLI was correctly rejected by the existing 32 MiB executable-input bound; the successful comparison used an earlier 13 MB release CLI, so it does not establish current-source shipping-binary identity.

A later dirty-tree source-transfer capacity check, including the new protocol and subsequent source changes, stays within both unchanged 32 MiB limits: 33,287,168 expanded carrier bytes and 33,154,665 logical bytes. Its log is retained at `artifacts/rust-rewrite/combined-v5-delivery-20261015-1/source-transfer.log`; the validation summary retains the earlier advisory transfer as historical evidence. The status-only report explicitly leaves descriptors, peak RSS and streaming unmeasured. Five paired numerical startup rounds on representative installed artifacts, the full declared workload matrix and all other performance/release gates remain open.

## Local continuity and validation checkpoint

A new RequestLease regression exercises a failed final-delivery verdict and a flushed control against real task/tool ownership state. Failed delivery releases the attempted Sonnet replacement and retains the previously confirmed Opus tool owner for the next continuation; successful flushing confirms the Sonnet replacement. The delivery verdict is supplied at the lease boundary, so this does not close the end-to-end socket-failure schedule or the remaining transport partials.

The third full default-parallel Rust workspace run passed 1,277 tests with nine opt-in tests ignored; strict workspace/all-target Clippy and formatting passed. The first two runs remain retained failures: each hit an existing 100 ms xtask child-output fixture deadline with zero output observed under parallel load. The final run uses a 5-second deadline only in the shared non-timing test fixture; production deadlines, byte limits and cancellation are unchanged. After the selected benchmark-report scope correction, both focused v5 tests, strict Clippy and the eight-row deterministic validation passed. The three required Node checks passed (596 tests and the installed package lifecycle); syntax for the revised Linux musl probe and `npm run check` were repeated after that probe edit. Fresh exact-head CI, numerical gates and shipping-binary qualification remain open.

## `efcbd66` CI checkpoint

The [exact-head CI checkpoint](../rust/parity/measurements/native-efcbd66-ci-checkpoint-summary.json) has ten terminal successes, two failures and one job still running at the recorded observation. All four Node jobs, the Linux x86/ARM and macOS ARM ordinary Rust jobs, both GNU glibc 2.28 jobs and the narrow i686 arithmetic job pass. The ordinary macOS Intel Rust job was in its direct installed-archive lifecycle. Both musl targets pass the revised Alpine index pins, offline build, static/transport checks, installed lifecycle and separate GNU-control audit. Their final independent mixed-artifact selection probe fails; neither complete musl job is qualified.

The added bounded witnesses narrow both musl failures to the probe's `serve_readiness` phase: the owned child exits with code 1 before a listening address appears. The probe explicitly pointed `AUTOROUTER_CONFIG` at a missing file, which the native CLI correctly rejects. The mixed npm archives' seven-suite installed smokes passed, but the failed independent probe did not establish which executable the dispatcher selected. The local correction removes that explicit missing-config override while preserving isolated fresh HOME/XDG directories; it needs a new CI run. The previously failed runs remain failed, and this checkpoint approves neither numerical performance nor release or default cutover.

## 2026-10-15 `e864131` and local follow-up checkpoint

The pushed `e864131` exact-head CI run passes all eight ordinary CI jobs, both GNU glibc 2.28 jobs and the narrow i686 arithmetic job. Both musl jobs pass native build, static and synthetic transport checks, installed archive lifecycle and separate GNU controls, then fail only the final independent mixed-artifact probe. The retained `artifacts/rust-rewrite/ci-e864-audit/e864-musl-{x86,arm}-dispatch-failure.json` witnesses show readiness and expected binary identity observed on both architectures, followed by HTTP 401 at the probe's unauthenticated `/health` request. Local source now sends a synthetic gateway token in that probe; it needs a fresh exact-head CI run. The failed musl jobs remain failed and no complete platform matrix is qualified.

Two local focused regressions pass on the later working tree. A synthetic HTTP/1 final-framing write failure occurs after the complete provider tool response is observed; the failed attempt leaves the previously confirmed Opus tool owner intact, while the flushed control confirms its Sonnet replacement. This exercises the gateway, observer and task state through an instrumented writer, not a real socket or TLS failure, and does not promote any of the 60 partial original definitions. The release packer also rejects qualifying unresolved architectures with preexisting or reused target names; its focused regression passes, but distinct names alone do not prove architecture semantics. The full local workspace run passes 1,279 Rust tests, strict Clippy and formatting; Node check, 596 tests and package smoke also pass. These working-tree changes are not part of the `e864131` CI result.

The frozen processing component's deterministic validation passes with matching output, decisions and evaluator/count calls and no provider calls (`artifacts/rust-rewrite/processing-e864131-20261015/validate/report.json`). Its separate normal-use 16 GiB Mac five-pair measurement has exact deterministic parity but an inconclusive median Rust/Node p95 ratio of 0.5287816363, interval 0.3517361073–0.7266943060, against the unchanged 0.5 ceiling (`artifacts/rust-rewrite/processing-e864131-20261015/numerical/report.json`). The report marks acceptance false: this component excludes HTTP pre/postprocessing and cannot qualify the whole gateway or representative-hardware gate. The same working-tree source transfer retains 33,300,480 expanded and 33,168,077 logical bytes under each 33,554,432-byte limit (`artifacts/rust-rewrite/processing-e864131-20261015/source-transfer-result.json`). Coverage stays 528 covered/60 partial; real evaluator quality, remaining workloads, full supported platforms, production release artifacts and default cutover remain open.

## 2026-10-15 release deadline and processing diagnostics follow-up

The [release-deadline supplement](../rust/parity/assertion-mappings/release-deadline-contracts.json) now maps all four assertions of frozen release-verification definition #16. Its retained source/native capture is hash-bound to the frozen original and current native sources (`artifacts/rust-rewrite/release16-promotion-20261015-1/manifest.json`). The Rust regression advances an independent elapsed clock as the metadata replies return, then verifies the exact `verification_deadline` result with no tarball or installation. The historical release mapping is unchanged; only this definition is promoted, bringing the ledger to 529 covered/59 partial.

The opt-in [processing stage diagnostic](../rust/parity/processing-stage-diagnostics.md) records six non-gating wall intervals on one fixed 8+32 synthetic Node/Rust pair. Its diagnostic and ordinary validation modes pass deterministic output, decision, payload and call checks. The diagnostic smoke used a debug helper, so its timing values do not qualify speed; it changes helper hashes, and historical numerical runs are not pooled with it. The post-change local suite passes 1,281 Rust tests, strict Clippy and formatting, plus Node check, 596 tests and package smoke. The current source transfer is 33,312,768 expanded and 33,180,213 logical bytes, below the respective 33,554,432-byte caps (`artifacts/rust-rewrite/post-d6b-20261015/source-transfer-result.json`). These changes still need exact-head CI and leave the performance, real-evaluator, platform, release and default-cutover gates open.

## 2026-10-15 release-check, Jev-deadline and subprocess follow-up

The [release-check CLI supplement](../rust/parity/assertion-mappings/release-check-cli-root-contracts.json) binds all four assertions of frozen definition #6 to a real built xtask executable invoked against a separate clean, tagged checkout. `release-check` now inspects its invoking checkout from the repository root or its `rust/` directory, preferring its own manifest even when the checkout itself is named `rust` inside another npm project. The synthetic test checks exact GitHub output after each success and no append after a malicious newline tag; its retained executables are immutable snapshots. Only this definition is promoted: the ledger is 530 covered/58 partial. The [Jev concurrency mapping](../rust/parity/assertion-mappings/router-concurrency-contracts.json) remains partial after a new non-paused 20 ms deadline regression, because the original never-settling JavaScript stream-cancellation callback remains unproven.

The [redaction timing supplement](../rust/parity/assertion-mappings/redaction-adversarial-bounds.json) completes only frozen redaction definitions #5 and #16. Their unchanged Node callbacks passed, and the release native test matched all 28 synthetic input/output identities and met each original strict two-second bound. The retained declaration and execution receipts describe a normal-use, battery-powered 16 GiB Apple M4. This finite-corpus result brings the ledger to 532 covered/56 partial; it does not establish asymptotic linearity, a speedup, whole-gateway performance or other-platform qualification.

Bounded native tool subprocesses now apply their existing output ceiling to stdout and stderr together. Initial and callback input use one pending action with partial writes in the main event loop, keeping cancellation, deadlines, stderr bounds and child exit responsive under stdin backpressure. Synthetic tests cover exact and overflowing limits on both streams, zero budgets, forced termination/reaping, blocked continuation writes and complete 1 MiB input delivery before stdin closes. Routine result metadata still excludes raw stderr and input; this does not close the original lifecycle helper's diagnostic-return contract.

The release-profile 8-warmup/32-sample stage diagnostic at `06e9420` retains deterministic output and call parity with no provider calls (`artifacts/rust-rewrite/stages-06e-20261015/release-diagnostic/report.json`). Its one normal-use pair observed pipeline p95 of 6.966 ms for Node and 3.770 ms for Rust; these instrumented component wall times overlap and cannot qualify the unchanged whole-gateway processing gate. The combined working-tree full Rust workspace run passes 1,288 tests with nine opt-in tests ignored; strict workspace Clippy and formatting, Node check, all 596 Node tests and installed-package smoke pass. The minimal-feature xtask suite separately passes 28 tests. The source transfer includes the new integration test and stays within both 32 MiB limits at 33,334,272 expanded carrier and 33,201,419 logical bytes (`artifacts/rust-rewrite/bounds-combined-20261015-transfer-result.json`). All 13 CI jobs for `06e9420` now pass; this newer working tree has no exact-head CI yet. Supported-platform breadth, real task quality, all measured gates, production artifacts and default cutover remain open.

## `a858a8d` PR merge-tree CI observation

The [retained terminal observation](../rust/parity/measurements/native-a858a8d-ci-summary.json) records twelve successes and one failure for PR head `a858a8d`, tested through merge checkout `e4c52c4`. All eight ordinary CI jobs, both GNU jobs, the narrow i686 job and x86 musl pass. ARM musl fails during the exact direct-archive installed lifecycle on the GNU host, before the mixed npm dispatcher probe. Its direct-help fixture expects exit 7 but observes 1; the other sixteen command tests pass. The original assertion omitted the alias and captured child output, so the cause is undetermined. A later diagnostic-only fixture change includes the alias, signal, fixed public-error classification and bounded synthetic output. It keeps the assertion and production behavior unchanged and needs fresh CI. This failure and the remaining full-plan qualification gates stay open.

## Executable evaluator handoff and buffered final-flush checkpoint

The [combined local evidence](../rust/parity/measurements/native-handoff-buffered-flush-summary.json) adds a target-specific executable evaluator archive with complete licenses, the frozen fixture and exact file/target/source identities. Its extracted receiver runs without Node, Cargo or a checkout and consumes the already verified fixture bytes. Independent frozen-label admission, path/byte/mode checks and restricted dispatch precede evaluation. Required release-profile tests exercise extraction, relocated help and synthetic cold plus thirty held-out calls. Supplied binary provenance remains `unverified_prebuilt`; this is a native-only trial tool, not a qualified release or a paired comparison. The [handoff guide](../rust/docs/local-model-comparison.md#ready-to-run-native-evaluator) keeps source/reference trials separate.

The [buffered final-flush controls](../rust/docs/openssl-transport-qualification.md) pass 24 schedules over HTTP, negotiated TLS 1.2 and TLS 1.3, with ordinary completion and attached terminal ownership. They retain real task/tool ownership until the final writer flush succeeds and verify resource cleanup after errors, cancellation and deadline expiry. These synthetic isolated-certificate JSON schedules do not establish full TLS policy, response-shape, pipeline or platform qualification. The executable inspector separately rejects opposite-endian ELF headers for every mapped target, without promoting target execution status.

The full default-parallel workspace passes 1,298 Rust tests with nine opt-in tests ignored; strict workspace Clippy, formatting, the 28-test minimal auditor, all 596 Node tests, Node contracts/style and exact JavaScript package smoke pass. The two required release bundle integration tests pass separately. A retained Mac ARM evaluator archive is 7,558,654 compressed bytes. The complete matched source/reference transfer is 33,409,024 carrier bytes and 33,275,972 logical bytes, under each unchanged 33,554,432-byte ceiling; an independent read compares all 687 actual public source files with the current tree. Coverage stays 530 covered/58 partial. Fresh CI is required for this checkpoint, and the historical ARM musl failure remains unexplained. The user has prioritized CI readiness before further rewrite expansion.

## `34c5142` CI readiness checkpoint

The [terminal PR merge-tree observation](../rust/parity/measurements/native-34c5142-ci-summary.json) records all thirteen jobs successful: four Node checks, four ordinary native Rust jobs, two GNU glibc 2.28 jobs, two musl jobs and the narrow i686 core check. All four ordinary native jobs pass the newly required executable benchmark bundle tests. Both musl jobs pass their exact installed archives and independent mixed-artifact dispatcher checks on GNU hosts. The previous ARM direct-help failure did not recur; its cause remains undetermined. Repository protections and the four required Node checks remain unchanged. Subsequent i686 runtime/CLI changes need their own CI and do not inherit this result.

## Additional i686 runtime and installed-CLI CI

The [host validation record](../rust/parity/measurements/native-i686-ci-host-validation-summary.json) accompanies a separate additive i686 runtime/CLI workflow. It inspects every discovered runtime and CLI test executable as genuine ELF32 EM_386, binds default and both installed-archive lifecycle runs through a private verifying Cargo runner, audits the ordinary release executable before helper feature unification, and preserves exact offline npm/direct installation checks. The owned Linux32 process personality makes real machine-name dispatch agree with the 32-bit Rust process on an x64 Ubuntu kernel. Existing thirteen jobs, repository protections, production deadlines and archive bounds remain intact. This does not qualify minimum OS/libc/kernel/CPU or promote Linux x86 from its unresolved, non-candidate status.

The host workspace passes 1,301 Rust tests with nine opt-in tests ignored, strict workspace and feature-enabled xtask Clippy, formatting, all 596 Node tests, contracts/style and exact npm archive smoke. The first concurrent Node attempts retain a startup timeout and temporary status-storage failure; sequential reruns pass with unchanged code and deadlines. The current matched transfer verifies all 687 actual allowlisted source files and the full reference restore at 33,412,096 carrier bytes and 33,279,279 logical bytes under the unchanged 33,554,432-byte caps. Genuine i686 execution and fresh exact-source CI remain pending at this recorded checkpoint.

The [first runtime32 run](../rust/parity/measurements/native-i686-ci-toolchain-failure-summary.json) at `0373631` stops before runtime builds: rustup resolves the short version under Linux32 personality to an i686-host compiler, which cannot load `libz.so.1`. The correction pins the full x86_64 host-qualified toolchain in commands and inherited `RUSTUP_TOOLCHAIN`, retaining the genuine i686 target and execution checks. It needs fresh CI; the original failed run remains failed.

The [host-qualified follow-up](../rust/parity/measurements/native-i686-ci-umask-fixture-summary.json) at `4396f49` passes the corrected compiler preflight, independent auditor, strict i686 Clippy and every runtime/CLI ELF32 inventory check. Actual i686 execution passes 487 runtime unit tests and fails one fixture: CI’s `077` umask masks its requested `0755` untrusted directory to `0700`, so cleanup correctly removes it. The test helper now explicitly sets its intended permissions; production logic and all assertions are unchanged. All 768 runtime/CLI tests pass locally under the same restrictive umask, with two opt-in tests ignored; formatting and strict package/all-target Clippy pass. Genuine i686 integrations, CLI and installed archives still need the next run.

## `5fa6ac8` fourteen-job CI checkpoint

The [terminal observation](../rust/parity/measurements/native-5fa6ac8-ci-summary.json) records fourteen successful jobs for the pushed source and its exact PR merge checkout. This includes the four protected Node jobs, all four native executable-bundle checks, both GNU and both musl jobs, the existing core arithmetic job, and the new i686 runtime/CLI job. Independent i686 artifact review verifies 45 ELF32 test executables, 59 exact runner receipts, 768 passing runtime/CLI tests and 78 installed lifecycle checks per npm/direct archive. Earlier failed and cancelled runs remain distinct. These successful synthetic checks do not establish full platform, real-task, performance, release or default-cutover approval.

## Exact-archive rollback CI and artifact admission

The [working-tree validation](../rust/parity/measurements/native-rollback-ci-validation-summary.json) adds the existing offline four-stage npm upgrade/rollback rehearsal to nine native packaging environments without changing the fourteen job names or required Node protections. The rehearsal now retains the same owned archive bytes that passed admission, closing the previous second-read replacement window. Real pending-archive fixtures cover replacement/removal and rejected inputs before output creation. A local Mac ARM rehearsal verifies the exact retained candidate, private configuration edits and unchanged schema1/2 history; its borrowed prebuilt CLI is explicitly unverified. Failed startup, partial writes, concurrent status ownership, direct rollback and the originally published baseline archive remain pending.

Release matrix validation now permits only the explicit predeclared `Linux x86` → `i686-unknown-linux-gnu` association to resolve its own baseline entry. Unrelated/shared target reuse and all review, evidence, scope, cap and qualification guards remain enforced. ARMv7 archive admission additionally requires EABI5 hard-float header flags; this does not qualify CPU instructions or a target runtime. No checked-in platform is promoted.

The complete local workspace passes 1,306 Rust tests, with nine opt-in tests ignored, and the minimal auditor passes 28. Formatting, strict workspace/feature-enabled xtask Clippy, all 596 Node tests, contracts/style and exact JavaScript package smoke pass. The real host rehearsal and all workflow/proof syntax checks pass. A matched transfer verifies all 687 current allowlisted inputs at 33,427,456 carrier bytes and 33,294,653 logical bytes under each unchanged 33,554,432-byte cap. These working-tree changes require fresh CI and leave the full-plan qualification and cutover gates open.

The [terminal `d9e88e6` checkpoint](../rust/parity/measurements/native-d9e88e6-ci-summary.json)
now records all fourteen configured jobs successful, including the exact npm
upgrade/rollback rehearsal in nine packaging environments. Independent artifact
inspection verifies all nine retained candidate identities. The frozen 0.5.2
baseline archive is byte-identical to the registry publication, closing that
specific identity gap. These results qualify only the recorded source and
synthetic environments; later changes require their own CI. Failed-startup,
partial-write, concurrent-ownership, direct-rollback and full-plan qualification
work remains open.

The [classifier and redaction checkpoint](../rust/parity/measurements/native-classifier-redaction-jev-summary.json)
rejects public classifier limits above the fixed ceilings before owning worker
state. New cancellation schedules prevent a successfully evaluated but cancelled
old generation from publishing over its same-key replacement. Real native HTTP
fixtures cover all eight frozen Jev body-failure calls under the unchanged live
20 ms deadline, plus retry, caller cancellation, body disposal and peer cleanup.
These additions preserve the separately documented JavaScript cancellation and
private-state API boundaries; they do not promote the concurrency definitions.
The full workspace passes 1,313 Rust tests with ten explicitly ignored tests or
helper fixtures, and the minimal auditor passes 28. Strict Clippy, formatting,
all 596 Node tests, exact npm package smoke and all seventeen differential
families pass. The source/reference transfer verifies 690 public inputs and
113 restored reference files at 33,484,800 carrier and 33,351,365 logical bytes
under the unchanged 33,554,432-byte ceilings. Fresh CI is required for this new
checkpoint; the full rewrite remains unqualified for release or default cutover.

## Buffered fingerprints and Intel CI allowance

The [fingerprint checkpoint](../rust/parity/measurements/native-turn-fingerprint-summary.json)
shares exact JSON fragment serialization with a 4 KiB SHA-256 batch, removing
catalog-sized preimage strings while preserving normalization and content aliases.
All 26 complete frozen Node cases pass, including a 4,263-byte normalized
composition and escaped scope/prompt identities. The full workspace passes
1,321 Rust tests with ten ignored tests or helper fixtures, strict Clippy,
formatting and the minimal auditor. All 596 JavaScript tests, contracts/style,
the exact 58-file package smoke, seventeen differential families and fresh
whole-connection deterministic validation pass. The matched source transfer
verifies 693 public inputs and 113 reference files at 33,548,800 carrier and
33,414,582 logical bytes under the unchanged 32 MiB limits.

The before/candidate five-pair measurements retain complete functional parity,
with native/reference p95 ratio estimates of 0.495 and 0.469. Both confidence
intervals cross the unchanged 0.5 limit. These sequential normal-use M4 runs
establish neither a causal improvement nor performance acceptance; the
[performance notes](../rust/docs/performance.md#buffered-turn-fingerprints)
retain their source and hardware limits. Coverage remains 532 covered/56 partial.

The [terminal `19e3d2c` CI observation](../rust/parity/measurements/native-19e3d2c-ci-timeout-summary.json)
records thirteen successful jobs and one cancelled Intel macOS job. GitHub's
annotation confirms the 45-minute job limit; checks after configuration parity
were skipped. The next native Intel job has a 60-minute allowance, with every
step retained and the other job/product/test deadlines unchanged. Fresh CI
must verify this source; neither the cancelled run nor local checks approve
release, the complete platform matrix or default cutover.

The [following musl setup observation](../rust/parity/measurements/native-4a60593-musl-index-summary.json)
finds both `4a60593` musl jobs stopped before product compilation. Independent
official-input inspection reproduces a blocker: Alpine's October 16 indexes
have different hashes, although all twelve explicitly asserted package versions
remain present. The reviewed refresh changes only those index digests and their
size/date metadata. Signature, package-version, image, compiler and offline
installation guards remain enforced. Actual package resolution and execution
require fresh CI; local signature verification and failed-step log inspection
were not performed. Routine musl CI still depends on a moving repository index;
freezing complete reviewed SDK images by immutable digest remains future work.

## Original Router real-clock deadline

The [narrow deadline supplement](../rust/parity/measurements/native-original-router-real-deadline-summary.json)
completes only original Router definition 11. The unchanged frozen Node callback
passes all four assertions. The new native real-clock replay uses the same
20 ms evaluator timeout and strict elapsed-time predicate below 1,000 ms,
checks the complete Sonnet fallback, and retries the same body through a fresh
successful Haiku evaluation. The existing paused-clock control remains pending
at 19 ms and completes after the final 1 ms advance. Each route checks cleared
classifier waiters before an awaited close joins advisory work and confirms
zero active synthetic requests.

All eight Router contract tests pass, followed by 1,322 workspace tests with
ten ignored tests or helper fixtures, strict Clippy and formatting. Node check,
all 596 tests and the exact 58-file installed package smoke pass. The matching
helper is byte-identical to the previous validated executable; the seventeen
differential families and whole-connection controls retain identical inputs.
The fresh source transfer verifies 693 public inputs and 113 logical reference
files, using 33,550,336 carrier bytes and 33,416,326 logical bytes under the
unchanged 32 MiB caps. Its supplied binary provenance remains unverified.

The capture runner's original failed report remains retained: it expected TAP
ordinal 11, while Node filtered the selected callback to ordinal 1. Independent
inspection verifies the same successful execution without rerunning or changing
the original callback. Neither process time nor TAP callback duration is the
private first-route elapsed value. This is an original finite scheduling
regression, with separate process ceilings; it does not qualify numerical
performance, the complete configuration/limit audit, supported platforms or
cutover. The historical main Router mapping is unchanged. At that checkpoint,
coverage was 533 covered/55 partial; Router definitions 49 and 50 still lacked a native
per-route counter boundary for the original callback and numeric outcomes.

## Complete `58e1531` CI checkpoint

The [terminal five-workflow observation](../rust/parity/measurements/native-58e1531-ci-summary.json)
records all fourteen named jobs successful. Git metadata binds the pushed
`58e1531` source and the PR merge checkout to the same tree. Intel macOS completes
all checks in 42m10s, including exact npm/direct installation and offline
upgrade/rollback. Both musl targets pass SDK/index admission, offline build and
lifecycle checks, their separate GNU controls and mixed-package selection.

This establishes the recorded CI scopes for that exact source. Per-job checkout
logs and artifact bytes were not downloaded or independently rehashed in this
observation. Earlier Intel timeout and musl setup failures remain retained; the
later timeout-regression commit and working-tree counter/transfer changes need
their own CI. Minimum deployment environments, full TLS/WSL, numerical and
real-provider gates, production provenance and default cutover remain open.

## Native per-route counting and lossless transfer reuse

The [counter callback supplement](../rust/parity/measurements/native-original-router-count-callback-summary.json)
completes only original Router definitions 49 and 50. The native library now
accepts an optional counter for one route, receiving the complete authoritative
document and selected target before HTTP projection. `None`, NaN and infinity
reach this boundary directly; only finite, nonnegative safe integers become
usable counts. Default HTTP counting retains its existing payload, cache and
deadline behavior. The [embedding guide](../rust/docs/embedding.md) documents
the native interface and its ownership/type differences from JavaScript.

The unchanged original Node callbacks pass all twelve assertion sites across
eight instances. Actual native replay verifies complete decisions, unchanged
bodies, exactly one custom callback and zero HTTP count requests per instance.
Additional tests prove start order/overlap, required inline work at speculative
capacity, exact deadline, ignored-token cancellation and owned future cleanup.
All nine new tests and all 53 Router tests pass. The full workspace passes
1,333 tests with ten ignored tests or helper fixtures, strict Clippy and
formatting. Node check, all 596 tests and the exact 58-file package smoke pass;
restricted loopback failures remain retained alongside successful authorized
runs. Fresh comparisons pass seventeen differential families and the
whole-connection deterministic control without numerical samples.

Transfer producers now explicitly use schema 4. The receiver also retains
schemas 1, 2 and 3. Two additional exact donors reuse the pinned license and
trusted baseline manifest, removing 99,840 physical carrier bytes while still
charging both logical copies. Every logical path, byte and mode remains
present; no filesystem links are introduced. Actual export/restore verifies
694 public inputs and all 113 logical reference files, at 33,495,040 carrier
bytes and 33,460,327 logical bytes under the unchanged 32 MiB limits. The same
helper restores the retained real schema 3 packet with exact bytes, ownership,
private directories and public-file modes. Fourteen format/filesystem tests
and the actual-export regression pass. Supplied-binary provenance remains
unverified; these checks do not approve a shipping artifact.

At that checkpoint, coverage reached 535 covered/53 partial, with every original
Router definition individually covered and all thirteen area statuses still partial. Historical
mapping/evidence identities remain unchanged. The counter/transfer checkpoint
needs fresh CI; full configuration/limits, transport/TLS/resource schedules,
numerical/real-task/hardware/platform and final cutover gates remain open.


## Stable policy-loader error category

The [policy error supplement](../rust/parity/measurements/native-policy-error-category-summary.json)
completes only original policy definition 3. Native policy loaders now return a
privately constructed `PolicyError` whose `code()` preserves
`AUTOROUTER_CONFIG_ERROR`. Safe messages remain unchanged; `Display`, `Debug`
and the standard error interface retain no rejected policy data, path or raw
I/O cause. Higher-level configuration and CLI adapters explicitly keep their
existing string-error interfaces. Fixed system paths and owner, mode, type and
symlink checks remain intact. The [embedding guide](../rust/docs/embedding.md)
documents the typed error migration.

The unchanged original callback passes three assertion sites across eleven
rejection instances. A native regression repeats all nine original trust/schema
cases, wrong ownership and an actual symlink through the filesystem loader,
checking category and diagnostic privacy alongside input preservation controls.
All three focused policy tests pass. The final workspace passes 1,334 tests with
ten ignored tests or helper fixtures, strict Clippy and formatting. Node check,
all 596 tests and the exact 58-file package smoke pass. Seventeen differential
families and the whole-connection deterministic control pass without numerical
samples or provider calls. The initial CLI callback type-check failure remains
retained alongside the corrected, successful full run.

Fresh schema 4 export/restore verifies all 694 public source inputs and 113
logical reference files at 33,503,232 carrier and 33,468,628 logical bytes under
each unchanged 33,554,432-byte cap. Independent review confirms archive hashes,
exact donors, source bytes and receiver ownership/mode/cleanup checks. Supplied
binary provenance remains unverified; these are local synthetic checks.

Coverage is now 536 covered/52 partial, with all thirteen areas still partial.
The original historical mapping and every other definition remain unchanged;
in particular, user-config definitions 6 and 12 still lack their own structured
error boundary. At that checkpoint, all configuration keys and limit rows
remained pending. Fresh CI for that source, complete transport/platform/resource and
performance/real-task/hardware evidence, final artifact provenance, rollback,
stabilization and default cutover remain open.


## Independent classifier work and subscriber ceilings

The [classifier limit review](../rust/parity/measurements/native-classifier-capacity-limits-summary.json)
completes only `pending_evaluations` (256) and `evaluation_subscribers` (1,024).
The native defaults retain both fixed ceilings. Constructor tests reject either
field above its ceiling before retaining a transport or creating worker state;
independent lower and zero values remain an explicit native embedding API.
Capacity fallback performs no extra evaluator request. The default Router uses
these same classifier limits.

The exact full-capacity schedules preserve all sixteen original observations,
covering 256 distinct evaluations and 1,024 subscribers sharing one evaluation.
They cancel and join every caller, reject late delivery, successfully retry the
overflow key and now await terminal close in both dimensions. Separate controls
cover independent waiter cancellation, reserved/retiring workers, same-key
replacement and abandoned-result cache isolation. Native work accounting keeps
reserved and retiring futures charged until destruction; this documented
ownership boundary is stronger than the source's pending-map-only admission.

The new saturation regression fills both lowered capacities with real mock
transport futures. Uncached same-key and new-key requests fall back, while an
already successful cached key returns without another request or any capacity
change. It cancels and joins every held caller and awaits the worker drain.
An already-cancelled token and terminal closure still precede cache lookup.
These facts concern classification admission, not confirmed provider execution.

All seventeen named capacity/cancellation tests pass. The unchanged original
Node callback separately passes eight assertion sites across both dimensions;
its previous raw control log is unavailable locally and remains recorded as
historical, without a rewritten success claim. The fresh control verifies all
111 frozen source identities before and after execution. The complete workspace
passes 1,335 tests with ten ignored tests or helper fixtures, strict Clippy and
formatting. Node check, all 596 tests and the exact 58-file package smoke pass,
as do seventeen differential families and the whole-connection deterministic
control. No numerical samples or provider calls are collected.

Fresh schema 4 export/restore verifies 694 public inputs and all 113 logical
reference files at 33,506,816 carrier and 33,472,483 logical bytes, within both
unchanged 33,554,432-byte caps. The supplied helper's provenance remains
unverified for shipping despite its actual local compiler/test binding.

The original-definition tally remains 536 covered/52 partial. Only two of the
53 limit rows advance; the other 51 limits, all 27 configuration keys and all
thirteen partial area statuses remain open. New-source CI, complete protocol,
TLS/platform/resource, performance, real-task/hardware and final artifact,
rollback, stabilization and default-cutover requirements remain separate.

## Complete 26412f2 CI checkpoint

The [terminal CI summary](../rust/parity/measurements/native-26412f2-ci-summary.json)
records all five runs and fourteen jobs passing for source `26412f2` and its
identical merge tree. Intel macOS completes in 29m35s; both musl architectures
pass SDK admission, locked dependency fetch, offline native and installed
lifecycle checks, followed by GNU-control and mixed npm-selection checks.
Required Node jobs and workflow timeouts remain unchanged.

This is terminal run/job/step metadata, without downloaded checkout logs or
independently rehashed CI artifacts. It does not qualify the subsequent local
policy/capacity changes, minimum deployment platforms, numerical performance,
real tasks, final release provenance, stabilization or default cutover. Earlier
CI successes and failures remain recorded separately.

## Empty-response delivery and direct constructor TLS

The [bodyless flush summary](../rust/parity/measurements/native-bodyless-flush-summary.json)
records 72 additional actual-constructor gateway schedules for empty JSON 200,
204 and 304 responses over HTTP, TLS 1.2 and TLS 1.3. Both ordinary and attached
terminal ownership are held at final client flush, with success, flush error,
cancellation and deadline expiry tested separately. The original 24 nonempty
schedules still pass. Clean response-body EOF occurs while the provider socket
remains held; it cannot establish downstream delivery. Successful empty delivery
releases the attempt once without confirming execution or replacing the prior
Opus tool owner. The other outcomes also preserve that owner.

Twelve direct-constructor controls separately exercise HEAD with a representation
length, zero-length GET, 204 and 304 over all three protocols. Exact method,
status, representation length, negotiated TLS and clean pool return are checked
before the held provider socket is released, without consumer body polling.
These controls preserve the gateway's existing method allowlist.

The complete Rust workspace passes 1,339 tests, with ten ignored tests or helper
fixtures, formatting and strict Clippy. Node check, all 596 tests and the exact
58-file package smoke also pass. Fresh bounded source export/restore
verifies 694 public inputs and 113 logical reference files at 33,517,568 carrier
and 33,483,160 logical bytes, under each unchanged 33,554,432-byte cap. The export
uses the retained, previously compiler-bound helper without a fresh-helper build
or shipping-provenance claim. No real provider calls or numerical samples are
part of this evidence.

The original-definition tally remains 536 covered/52 partial. No further limit,
configuration or area status advances. At that checkpoint, final-data end hints,
delayed trailers, close-delimited EOF and broader reuse ownership remained among
the actual-constructor lifecycle requirements; ordinary production transport still
uses Rustls.
The pushed `2d5defa` checkpoint has a separate ARM musl SDK-preparation failure
before Rust compilation, with diagnosis pending. Its CI cannot qualify these
later local tests. All other platform, performance, real-task, provenance,
rollback, stabilization and cutover requirements remain open.

## Consumer-owned pool return and retained data

The [consumer/pool summary](../rust/parity/measurements/native-consumer-pool-handoff-summary.json)
records six actual-constructor controls over HTTP, TLS 1.2 and TLS 1.3. After
clean source-body EOF, an unread response keeps its raw connection reserved.
Taking the final data frame returns that reservation using its end hint, without
an extra EOF poll. Request B is then read on the same accepted provider socket.
A retained clone of A's bytes remains charged through B and client/task/cache
cleanup, and the last clone's drop releases the remaining buffer ownership.

The abandonment control drops prefetched A without a body poll. Its reservation
and socket retire before B uses a newly accepted peer. Both outcomes verify
request methods, paths and marker headers, response status, length and payload,
negotiated TLS, assignment identities and bounded task/socket/cache/buffer
cleanup. Existing primitive and empty-response tests
remain unchanged.

Formatting, strict Clippy and the complete workspace pass 1,341 Rust tests, with
ten ignored tests or helper fixtures. Node check, all 596 tests and the exact
58-file package smoke pass. Fresh bounded export/restore verifies 694 public
inputs and 113 logical reference files at 33,528,320 carrier and 33,493,812
logical bytes, under each unchanged 33,554,432-byte cap. The retained helper has
no fresh-build or shipping-provenance qualification; no numerical samples or
real provider calls are collected.

Only the gateway test inventory gains two references. All 588 original rows
remain 536 covered/52 partial; all configuration/limit rows and partial area
statuses are unchanged. Actual gateway final-data ownership, delayed trailers,
close-delimited EOF and broader concurrent reuse remain open, along with the
full platform, performance, task-quality, provenance, rollout and cutover gates.
These later local sources receive no qualification from the separate `2d5defa`
CI runs or their unresolved ARM musl SDK-preparation failure.

## `2d5defa` terminal CI checkpoint

The [five-run terminal metadata record](../rust/parity/measurements/native-2d5defa-ci-summary.json) binds all fourteen jobs to source `2d5defa` and the matching merge tree. Thirteen jobs pass, including Intel macOS, both GNU jobs, x64 musl and both i686 jobs. ARM musl fails during SDK setup before compilation; permitted annotations report only exit code 1, so its exact cause remains unproven. No job logs or binary artifacts were downloaded. This checkpoint remains failed, and its results do not transfer to the later bodyless, consumer-pool or configuration changes.

## Saved-configuration error categories

The [per-assertion supplement](../rust/parity/measurements/native-user-config-error-category-summary.json) completes only original user-config definitions #6 and #12. Public native path/load APIs now expose the stable configuration-error code through `UserConfigError`, with safe built-in diagnostics and no nested cause. CLI messages, save APIs and policy-first ordering stay unchanged. Arbitrary injected Keychain messages remain verbatim and are explicitly outside the built-in sanitation guarantee. The [embedding guide](../rust/docs/embedding.md) records the native error-rendering migration.

The two unchanged original callbacks pass their ten assertion sites across 82 statically expanded checks. Their test child records zero policy-file reads under mocked optional-policy absence. The original wrapper still fails its two-trace receipt criterion, and the coordinator exit status remains unmeasured; the source-based isolation review does not turn that wrapper into a pass. The native suite passes 1,342 tests, with ten opt-in tests ignored, alongside strict Clippy, formatting, 596 Node tests, package checks, seventeen differential families and deterministic whole-connection parity. The fresh Cargo-bound source-transfer proof verifies all 694 native inputs and 111 frozen files within both unchanged 32 MiB limits. Coverage is 538 covered/50 partial; all configuration-key, limit and area statuses remain unchanged. Current-source CI, performance, platform, release and cutover qualification remain open.

## URL configuration primitive conversion

The [URL conversion supplement](../rust/parity/measurements/native-config-url-coercion-summary.json) fixes 70 reproduced mismatches among 167 declared raw-value cases. Public native configuration now converts URL primitives and arrays before applying the existing endpoint safety checks. Document input reads original nonfinite numbers and UTF-16 at that boundary, preserving inactive-backend masking and first-error order. Credential handling remains a separate open issue.

All 627 configuration/auth cases match the frozen reference, including the byte-identical original 460-case prefix. Two native regressions cover 43 document controls with separately executed original outcomes. The expected-red control retains its actual failing binary and logs, but exact reconstruction of its test-only source is unverified. Full local validation passes 1,344 Rust tests, strict Clippy, formatting, 596 Node tests, package checks, seventeen differential families with 206,792 matches and deterministic whole-connection parity. These checks contain no numerical performance qualification.

The unchanged source-transfer limits pass with all 694 native inputs and 111 frozen files retained; carrier headroom is 2,560 bytes. No source/fixture removal, minification or manifest compaction was used. Coverage remains 538 covered/50 partial, with all definition, configuration-key, limit and area statuses unchanged. This working tree still needs its own CI checkpoint; platform, performance, real-provider, release and cutover gates remain open.

## Terminal CI for c86bd78

The [five-run terminal metadata record](../rust/parity/measurements/native-c86bd78-ci-summary.json) reports all fourteen jobs successful for source `c86bd78` and its matching merge tree. This includes four protected Node jobs, four ordinary Rust hosts, GNU and musl on x64/ARM, and both i686 workflows. Intel completes in 37m57s, including exact npm/direct installation and upgrade/rollback steps. Both musl SDK and later native/GNU-control phases succeed.

This is job/step metadata evidence; no executable, archive or report bytes were independently downloaded or rehashed. It does not transfer to the later URL or credential changes, diagnose the historical `2d5defa` ARM SDK failure, or approve the full platform matrix, performance, provider quality, production provenance or cutover.


## Credential source preservation and indexed source transfer

The [credential-source review](../rust/parity/measurements/native-config-credential-source-summary.json) retains absence, null, original number types and UTF-16 for the three credential fields. Required-key validation keeps backend activation and Jev-before-Anthropic ordering. Explicit replacement methods clear captured state; evaluator snapshots erase unrelated Anthropic/local secrets while keeping effective Jev cache identity. Native error and owned-value interfaces remain documented migrations, without claiming arbitrary JavaScript alias or Error identity.

All 360 targeted configuration cases now match, fixing 214 earlier mismatches. They are appended to the public generator after the byte-identical 627-case prefix. Full local validation passes 1,356 Rust tests, strict Clippy, formatting, 596 Node tests, package checks and seventeen differential families with 207,152 matches. Deterministic whole-connection parity passes without numerical samples or provider calls.

The separate 376-case corpus still has ten failures in child-environment adaptation. Sixteen actual frozen-source child processes confirm that null and non-string local tokens reach the OS as strings; the native adapter currently drops them. Fifty-five additional source controls retain failure ordering, overflow and UTF-16 observations for the next fix. These are genuine pending conversions, rather than approved representation differences. Malformed credential transport remains unqualified.

The original schema4 export and first prefix-coded schema5 attempt both fail the unchanged carrier cap and remain recorded failures. The revised paired schema5 index independently commits the complete path list and retains every full file digest; raw members, fixtures, modes and byte accounting remain intact. A fresh Cargo-bound helper and independent decoder verify all 694 native inputs and restore all 113 logical reference files, including the 111 frozen files. Carrier size is 33,546,240 bytes and logical payload is 33,511,606 bytes under each 33,554,432-byte limit. Default source-only JSON manifests and historical transfer readers stay unchanged. This proves source transfer, not shipping binary provenance.

Coverage remains 538 covered/50 partial, with all 588 definition rows, 27 configuration keys, 53 limit rows and thirteen area statuses unchanged. New-source CI, performance, platform, real-provider, final artifact and cutover qualification remain open.


## Pushed URL checkpoint CI completion

The [terminal metadata record](../rust/parity/measurements/native-2ebc34b-ci-summary.json) reports all fourteen jobs successful across five workflows for `2ebc34b` and its matching merge tree. Intel finishes in 37m47s; both GNU, both musl and both i686 workflows pass. Root independently reparsed all fourteen actual job outcomes and rehashed the five primary run records and source/merge metadata. These metadata observations do not independently verify downloaded artifact bytes or qualify platforms, performance or release. The newer `a700a4b` credential/source-transfer commit is pushed to PR #16 and has its own fresh CI; local child-environment and schema6 changes remain separate and unqualified.

## Child environment materialization and schema6 transfer

The [environment conversion review](../rust/parity/measurements/native-child-environment-source-summary.json) preserves effective local credentials until the OS boundary. Null, booleans, numbers, arrays, objects and UTF16 now receive the observed string conversion; explicit replacements clear captured values. The fallible native builder retains unrelated OS bytes and returns safe conversion/NUL errors without credential-bearing causes. Launcher cleanup remains owned, and live/context tools prepare the environment before opening transport or listeners.

Two native regressions execute 54 actual children and eight private failure controls. All sixteen native environment projections match retained actual Source OS observations. The older pure-builder corpus matches 371 of 376 rows: five API-key values retain their raw types in the JavaScript object before spawn, while the native OS map contains strings. These are documented API representation differences; the whole pure corpus is not reported as passing. Error identity, failure phase and arbitrary parent insertion-order failure precedence remain native API limits. Effective Jev provider-header conversion and malformed local-gateway transport remain pending.

Formatting, strict workspace/all-target Clippy and all 1,363 Rust tests pass, with eleven ignored helper/opt-in entries. Node checks, all 596 tests, the exact 58-file package smoke, seventeen differential families with 207,152 matches and deterministic whole-connection parity also pass. These are synthetic local checks, without numerical performance qualification or real provider calls.

The first current-source schema5 export fails its unchanged cap and remains retained. Schema6 uses canonical big-endian binary CPIO while retaining the indexed manifest, every raw source/reference byte, full path/hash commitment, modes and both separate 33,554,432-byte limits. Fresh export, independent decoding, external BSDtar reads and restoration verify all 694 native inputs and all 113 logical reference files at 33,521,152 carrier bytes and 33,546,386 logical bytes. Actual retained schema3/4/5 transfers each restore all 113 reference files; legacy readers and default source-only JSON remain unchanged. Shipping binary provenance remains unqualified.

Only test inventories gain references: 538 definitions remain covered and 50 partial. All 27 configuration keys, 51 pending limit rows and thirteen partial areas remain unchanged. Current-source CI, the unresolved prior ARM test failure, complete platform/TLS/resource, performance, real-task/provider, release and cutover gates remain open.

## Terminal a700a4b CI and diagnostic checkpoint

The [terminal metadata record](../rust/parity/measurements/native-a700a4b-ci-summary.json) retains thirteen successful jobs and one failed job across five workflows for `a700a4b`. Intel macOS, both GNU, both musl, both i686 and all four Node jobs pass. Ordinary Ubuntu ARM workspace tests fail with exit 101; the available check annotation does not name a test or establish its cause. No CI logs or executable/archive artifacts were downloaded or independently rehashed. These observations apply to the recorded source and matching merge tree only.

The later `0fb3115` checkpoint pushes the locally validated child-environment/schema6 changes and adds bounded, best-effort failed-test-name annotations. Thirteen synthetic diagnostic controls preserve Cargo arguments, stream bytes and exit status, including parser failure; the parser also extracts exactly the two test names from the retained failed local workspace run. The new workflows have started and remain unqualified. Uncommitted Jev header and process-fixture work is excluded from that push. All full-plan platform, performance, provider, provenance, rollout and cutover gates remain open.

## Jev credential headers and fixture admission

The [credential-boundary checkpoint](../rust/parity/measurements/native-jev-authorization-summary.json) fixes effective Jev authorization conversion at each request. Two regressions retain all 44 raw credential cases and six replacement sequences, totaling 80 evaluations with 64 mock requests and 16 pretransport errors. Frozen Source observations include a separate actual owned HTTP probe for six Latin1 header controls and three rejected control-byte values. Pre-cancellation, safe errors, configuration immutability, deadlines and response bounds remain intact. Native serialized-state/error ordering and malformed Anthropic/local-gateway authorization remain explicit limits.

The first full run retains two process-fixture failures with missing readiness markers; their cause is unproven. The fixtures now separate bounded startup admission from the active cleanup scenario, atomically publish readiness and hold actions behind an armed gate. Negative controls distinguish missing readiness from an actual failed gate creation. The public subprocess deadline and ownership monitor retain their prior behavior, and every original assertion remains. The retained escaped-marker check remains bounded negative cleanup evidence. Shared test helpers preserve all cases and ownership schedules.

Fresh local validation passes 1,367 Rust tests with eleven ignored entries, 29 minimal-auditor tests, strict workspace/minimal Clippy, formatting, 596 Node tests and the exact 58-file JavaScript package check. Seventeen differential families match all 207,152 cases; whole-connection parity passes with numerical performance explicitly unmeasured. Independent export/restore checks retain all 694 native inputs and 111 frozen originals (113 logical reference files), using 33,528,832 carrier bytes and 33,554,214 logical bytes within each unchanged 33,554,432-byte cap. No source or fixtures were removed or minified.

Only five test-inventory references are added. Coverage remains 538 covered/50 partial, with all 27 configuration keys, 51 pending limit rows and thirteen partial areas unchanged. These source bytes require their own CI checkpoint. Performance, full platform/TLS/resource, real-task/provider, final artifacts/provenance, rollback, stabilization and default cutover remain open.

## Terminal CI for the streamed classifier hash checkpoint

The [terminal metadata record](../rust/parity/measurements/native-7b419cb-ci-summary.json) reports all fourteen jobs successful across five workflows for `7b419cb` and its matching merge tree. Intel macOS finishes in 35m57s; GNU, musl, both i686 workflows and all four protected Node jobs pass. Root independently verified the retained primary metadata and source-tree binding. These observations qualify that CI checkpoint only; executable/archive bytes were not downloaded or independently rehashed, and later working-tree changes require separate checks.

The [streamed hash measurement](../rust/parity/measurements/native-processing-hash-stream-summary.json) observes a processing p95 ratio of 0.4419 on the normal-use 16GB M4. The declared local target is met, while representative hardware, complete performance acceptance and shipping qualification remain open. This measurement belongs to its recorded source and does not transfer to subsequent credential changes.

## Anthropic credential boundaries: local work in progress

The [local credential checkpoint](../rust/parity/measurements/native-anthropic-header-summary.json) separates Node HTTP forwarding from Fetch-style advisory counting. It preserves raw credential conversion, repeated fields and omission, and moves forwarding rejection after request validation and routing. Invalid count headers retain an unavailable callback so classification and context policy still execute. Subscription authentication and current request OAuth headers remain owned by the existing gateway path.

Seven added regressions include 39 source-derived header profiles, 51 actual native header-wire exchanges, six earlier rejection gates, three inference-order cases, three product-router count cases and retention of a previously confirmed Opus tool owner. The full workspace run records 1,372 passes and two failures across 66 groups, with eleven ignored entries. One new test incorrectly expected Haiku for an image request with unavailable counting; an actual frozen JavaScript control confirms the existing Sonnet capacity promotion. Correcting that expectation leaves the other 694 native inputs unchanged, and the six-test gateway module then passes. The recorded full run remains failed.

The remaining failure is the unchanged 32 MiB logical source-transfer limit. A fresh helper export also rejects it before creating a destination; carrier fit remains unqualified. Source or fixture removal, minification and cap increases were not used. Formatting, strict workspace/minimal Clippy, 29 minimal-auditor tests, the three Node checks, all 17 differential families (207,152 matches) and deterministic whole-connection parity pass. No numerical measurement or provider call was made for this working tree. The embedding guide documents the additional native credential/count APIs.

This work remains local while the source-transfer gate is resolved. Coverage stays at 538 covered/50 partial, all 27 configuration keys and 51 limit reviews remain pending, and all thirteen areas remain partial. Full-plan platform, protocol/resource, performance, provider/task quality, provenance, rollback, stabilization and default-cutover gates stay open; JavaScript 0.5.2 remains the shipping default.

## Shared compatibility policy: local work in progress

The [shared-guard checkpoint](../rust/parity/measurements/native-shared-compatibility-guard-summary.json) replaces the duplicate serde and original-document compatibility policies with one engine over borrowed nodes. UTF-16 access, exact model identity and representation-specific thinking/equality adapters remain explicit. There is no eager conversion, subtree cloning or serialization. The complete existing routing-test suffix is byte-identical; its cross-entry assertions now check adapters over shared policy, while the frozen JavaScript differential remains the independent policy oracle.

Two small test-helper extractions retain the original JSON comparisons, manifest reads and restored-byte/mode assertions. The five changed native inputs save 14,906 raw source bytes. Every source and fixture file remains. Formatting, strict workspace/minimal Clippy, 29 minimal-auditor tests, all 17 differential families with 207,152 matches, deterministic whole-connection parity and the three Node checks pass, including 596 tests and the exact 58-file JavaScript package. Whole-connection validation retains no numerical samples and does not qualify performance.

The fresh full workspace records 1,373 passes, one failure and eleven ignored entries across 66 groups. Only the source-transfer cap test fails; the corrected credential and confirmed-owner regressions pass. The earlier sandboxed run and first Clippy failure remain separate retained evidence. An actual export from the fresh Cargo-bound helper still rejects the unchanged 32 MiB logical cap before creating a destination. A lower-bound forecast remains 18,941 bytes over before metadata growth; carrier fit and restore are unqualified.

This increment remains uncommitted and has no current-source CI, performance, provider or release qualification. The 538 covered/50 partial definitions, 27 pending configuration keys, 51 pending limit reviews and thirteen partial areas remain unchanged. All full-plan qualification and cutover gates remain open; JavaScript 0.5.2 is still the shipping default.

## Original-node readers and fixture setup: local checkpoint

The [shared-node checkpoint](../rust/parity/measurements/native-shared-original-node-summary.json)
centralizes original-node truthiness, exact UTF-16 string predicates and ordered
property paths, plus repeated CLI and gateway fixture setup. Representation,
callback, observer and lifecycle policies remain explicit. Existing fixture
bytes and test assertions remain. Twenty native inputs save another 6,202 raw
source bytes; no source or fixture files were removed or minified.

Strict workspace/minimal Clippy, formatting, 29 minimal-auditor tests, all
207,152 differential cases across seventeen families, deterministic whole-connection
validation and the three Node checks pass, including 596 tests and the exact
58-file package smoke. The full workspace records 1,373 passes, one source-transfer
failure and eleven ignored entries. The initial compile failure from three stale
helper calls remains retained; the corrected source has its own validation.

Actual export from the fresh Cargo-bound helper still fails the unchanged
32 MiB logical limit before creating a destination. The lower-bound forecast is
12,739 bytes over before metadata growth; carrier fit and restore remain
unqualified. This source remains local and receives no CI, numerical performance,
provider/task, release or cutover qualification. All coverage, configuration,
limit and area statuses remain unchanged.

## Raw configuration documents: reproduced gaps

The [raw-input checkpoint](../rust/parity/measurements/native-raw-config-document-summary.json)
executes 82 declared synthetic environments through the actual frozen JavaScript
`readConfig` and compiled native `read_config_document`. Both children exit
successfully; all original input kinds, UTF-16 units and before/after trees
match, all 111 frozen files remain unchanged, and the Source probe observes no
network attempts. Only 48 outcomes match; 34 remain genuine differences.

Twelve invalid-number inputs select native defaults, three expose a different
timeout diagnostic, six change first-error ordering, ten substitute UTF-16 units
in successful scalar configuration output, two reject accepted deep keep-alive
arrays and one reports the wrong primitive-conversion error. This experiment
concerns the raw embedding API, not ordinary CLI environment strings. The finite
987-case configuration/auth corpus and existing execution identity sidecars do
not establish correctness for these additional inputs or full serialization.

The initial probe-link failure was an inherited harness file-size limit; its
logs and original runner remain retained. Correcting only that compile limit
leaves runtime/output/deadline bounds intact. No configuration fix or coverage
promotion is claimed. The next implementation needs borrowed original-kind
validation plus safe owned-value propagation, without a deep recursive Value,
truncated success or a newly narrowed input domain.

## Shared audit lifecycle: latest local checkpoint

The [audit-lifecycle checkpoint](../rust/parity/measurements/native-shared-audit-lifecycle-summary.json)
shares the decision/outcome fixtures' owned state and cleanup methods. Both
specialized constructors, callback policies and complete assertion/capture
suffixes remain byte-identical. Only three integration-test inputs change from
the preceding checkpoint, saving 2,844 raw bytes; every product-source byte is
unchanged.

Formatting and strict workspace Clippy pass. The complete workspace again
records 1,373 passes, one source-transfer failure and eleven ignored entries.
The unchanged-cap export also fails before creating a destination; its retained
helper is not a fresh build or shipping-provenance proof. The lower-bound size
forecast is 9,895 bytes over before metadata growth. Earlier differential, Node
and deterministic whole-connection results retain their original source binding
and were not repeated for this test-only change.

All 538 covered/50 partial definitions, 27 pending configuration keys, 51 pending
limit reviews and thirteen partial areas remain unchanged. Source-transfer fit,
current-source CI, full platform/resource/performance/provider qualification,
release provenance, rollback, stabilization and default cutover remain open.
JavaScript 0.5.2 remains the shipping default.

## Original configuration values: latest local checkpoint

The [owned-value checkpoint](../rust/parity/measurements/native-config-owned-values-summary.json)
uses one borrowed parser for typed and original-document inputs, retaining
accepted keep-alive values in compact owned arenas. Evaluator requests, cache
identity, warmup, configuration reports and routing reports use those effective
values. Explicit replacement methods clear captured state; existing exact model
overrides retain their documented authority. Four actual frozen JavaScript cache
controls supply independent regression expectations.

A separately declared, freshly Cargo-bound JSON-text experiment matches all
82 original synthetic inputs semantically, in property order and in literal
serialized text. Both children complete with 27 successful serializations,
55 matching configuration errors and no serialization errors; inputs and all
111 frozen files remain unchanged. This fixes the reproduced parser/output
differences for that corpus. The historical 48-match/34-mismatch Value experiment
remains unchanged. Its unchanged native harness rerun now exits 2 after 63 rows
at the first unpaired UTF-16 serialization error; that Value API migration is
not reported as old-observer parity. No provider calls or measurements occur.

Strict workspace/all-target Clippy passes. The actual full workspace records
1,386 passes, two failures and eleven ignored entries across 66 groups. The
unchanged source-transfer limit still fails. A callback-input backpressure test
also reaches its outer deadline; a possible fixture wait after child completion
is identified, but this run does not establish the cause. Neither failure is
discarded or treated as passing. The source changes add 43,913 raw bytes,
leaving a conservative logical-transfer overage of at least 53,808 bytes before
metadata growth. Both separate 32 MiB transfer caps remain unchanged.

This source remains local and has no current-source CI, full performance,
platform, provider/task-quality, release or cutover qualification. The ledger
still has 538 covered/50 partial definitions, 27 pending configuration reviews,
51 pending limit rows and thirteen partial areas. JavaScript 0.5.2 remains the
shipping default.

## Subprocess waiter and Auto output limits: latest local checkpoint

The [waiter and output-limit checkpoint](../rust/parity/measurements/native-callback-auto-output-summary.json)
fixes a test interrupt waiter that could remain pending after its owned child
finished without invoking a callback. A new three-mode silent-child regression
checks exit, reaping and absence of false stop flags. The original backpressure
assertions and deadlines remain unchanged. Both tests pass in the fresh full
workspace; the earlier timeout is retained with its original cause unproven.

Shared configuration-test setup and three TLS fixtures save 2,234 formatted
source bytes. Peer read/finish/scenario policies and assertion obligations remain;
sender cloning, admission limits and TLS resumption stay explicit. The fresh
workspace records 1,389 passes, one source-transfer failure and eleven ignored
entries. Formatting, strict workspace/minimal Clippy, 29 minimal tests, all
207,152 differential cases across seventeen families, and deterministic
whole-connection validation pass. Node checks, 596 tests and the exact 58-file
package smoke also pass. Numerical performance remains unmeasured here.

Fresh execution of the unchanged 23 frozen Auto definitions retains 374 actual
Source calls, 372 captured JSON cases and both historical non-JSON boundaries.
All 372 cases match the freshly Cargo-bound native helper. A separate Source
probe exercises all twelve original output-limit arguments before serialization;
the native supplement verifies original Infinity and unchanged number bits.
NaN is explicitly outside the supported native JSON/document representation.
Independent review accepts that specific API difference without claiming a
native NaN call or replacing it with null. Only Auto definition 16 is promoted:
539 definitions are covered, including that reviewed migration, and 49 remain
partial. All other definition rows, 27 configuration reviews, 53 limit rows and
thirteen partial areas retain their previous status.

The transfer still exceeds its unchanged 32 MiB logical limit, with a conservative
overage of at least 57,004 bytes before metadata growth; the added regressions
outweigh this increment's sharing savings. Carrier fit and restore remain
unqualified. Current-source CI, complete platform/protocol/resource/performance,
provider/task-quality, final artifact provenance, rollback, stabilization and
default cutover remain open. JavaScript 0.5.2 is still the shipping default.

## Observer fixed limits and owned-process EOF: latest local checkpoint

The [observer-limit checkpoint](../rust/parity/measurements/native-observer-limits-summary.json)
adds two native tests with 44 schedules for the exact 65,536/65,537-byte and
256/257-block boundaries. All 25 observer tests pass. A separate frozen Source
probe passes 39 controls, and all 39 complete Source/native results match.
Actual body tests verify original byte-buffer pointers and content, internal
bounds, suppression before clean EOF, and cleanup on finish or drop. The pure
fixture adapter's copied forwarding field is not used as body-delivery proof.

Independent review accepts only the two fixed observer limit rows: four limits
are now covered and 49 remain pending. All 588 definition rows remain unchanged
at 539 covered and 49 partial; the 27 configuration and thirteen area reviews
retain their prior statuses. This does not qualify every constructor input or
physical memory usage.

The broader workspace stops after 1,088 passes, one failure and four ignored
entries. The owned-process timeout test observes its timeout, forced kill and
SIGKILL, but its output-EOF assertion fails. Safe assertion diagnostics were
added without changing production, the shell workload, deadlines or predicates.
One focused diagnostic run passes; the original full-run failure remains
unexplained and unresolved. The source-transfer test was not reached in this run.

Strict workspace Clippy, formatting, Node checks, all 596 Node tests and the
58-file package smoke pass. An independently reviewed offline census computes
33,622,992 logical payload bytes and 33,597,952 expanded carrier bytes, exceeding
the unchanged caps by 68,560 and 43,520 bytes respectively. These are size
calculations; native export, restore and compressed archive hashes remain
unqualified. Updated status metadata is outside the existing transfer allowlist.
The [configuration boundary checkpoint](../rust/parity/measurements/native-config-key-boundaries-summary.json)
now passes those 41 predeclared numeric-boundary and isolated backend-validation
controls against the frozen Source: 12 complete configurations and 29 exact
errors match. All 27 configuration-key reviews remain pending; these controls
do not qualify every input or downstream consumer. Current-source CI,
platform/performance/provider qualification, artifact provenance, rollback,
stabilization and cutover remain open. JavaScript 0.5.2 remains the shipping default.


## Configuration selectors and shared CLI fixtures: latest local checkpoint

The [CLI fixture and EOF diagnostic checkpoint](../rust/parity/measurements/native-cli-fixture-and-eof-diagnostic-summary.json)
shares the existing local-diagnosis response protocol and exact saved-config byte
reads across five CLI test files. All original test names, assertions, profiles,
workloads and deadlines remain. Formatting, strict CLI Clippy and all 42 affected
tests pass. The formatted change saves 1,341 raw logical bytes. Independent offline
arithmetic now charges 33,621,651 logical bytes and 33,596,416 expanded carrier
bytes, still exceeding the unchanged caps by 67,219 and 41,984 bytes. No native
export, restore or compressed archive qualification is inferred.

One separately declared guarded diagnostic of the original timeout test passes.
Both empty pipes reach EOF on the first original tail round; no interrupted reads
or emergency intervention occur, and the guard joins cleanly. Temporary test-only
instrumentation is removed byte-for-byte after the run. This does not reproduce,
explain or fix the retained full-workspace output-EOF failure. The existing shell,
one-second timeout, 30 ms grace, 64 drain rounds and four assertion predicates
remain intact.

The [individual selector review](../rust/parity/measurements/native-enum-configuration-keys-summary.json)
accepts only `AUTOROUTER_AUTH_MODE`, `AUTOROUTER_EVALUATOR` and
`AUTOROUTER_SESSION_LOG_MODE`. It composes specific executed Source/native controls,
16 named historical consumer passes and explicit source review of exact string
and null/default rules. Unexecuted extra raw-input and provenance permutations
remain documented. The [numeric-key review](../rust/parity/measurements/native-numeric-configuration-keys-summary.json)
separately accepts `AUTOROUTER_PORT` and `AUTOROUTER_TOKEN_COUNT_TIMEOUT_MS` using
29 specific boundary outcomes, four raw nonfinite controls and their actual
consumers. The Jev 20 ms replay is explicitly excluded from count-timeout proof;
the separate default-counter 15 ms and custom-count 20 ms tests supply that evidence.
Unexecuted OS-port, wall-time and provenance permutations remain explicit. Five
configuration keys are covered and 22 remain pending; all 588 original definition
rows remain at 539 covered and 49 partial, all 53
limits remain at four covered and 49 pending, and whole-area approval is unchanged.

Current-source CI and full workspace qualification remain open alongside the
platform, performance, real-task, provenance, rollback, stabilization and cutover
gates. Changes remain local; JavaScript 0.5.2 remains the shipping default.

## Shared runtime fixtures: latest local checkpoint

The [runtime fixture checkpoint](../rust/parity/measurements/native-runtime-fixture-sharing-summary.json)
shares seven identical loopback listener/origin constructors and eighteen runtime
test configuration constructors. The Value and document configuration APIs remain
separate. These changes affect test setup only; original inputs, assertions,
workloads, deadlines and product behavior remain unchanged. Formatting and strict
runtime/all-target Clippy pass. The focused TLS suite passes all 197 tests, followed
by all 529 runtime unit tests on the combined source. The TLS tests are part of
the runtime total, rather than additional unique tests.

The formatted changes save 2,195 raw logical bytes: 801 from listener setup and
1,394 from configuration setup. Exact offline arithmetic now charges 33,619,456
logical bytes and 33,594,368 expanded carrier bytes, exceeding the unchanged
32 MiB caps by 65,024 and 39,936 bytes respectively. Native export, restore and
compressed archive qualification remain open.

No coverage row is promoted by these refactors: 539 original definitions are
covered and 49 remain partial; five configuration keys are covered and 22 remain
pending; four limits are covered and 49 remain pending. All thirteen areas retain
partial status. Earlier source-bound reports remain historical: test-only edits
change whole-file hashes, and retained preimages preserve those original sources.
They do not establish fresh full-workspace or current-source CI qualification.

The original owned-process output-EOF failure remains unresolved. A broader
instrumented cohort is proposed but has not run. Supported-platform, performance,
real-task, artifact provenance, rollback, stabilization and default cutover gates
remain open. Changes remain local; JavaScript 0.5.2 remains the shipping default.

## Flag configuration and owned-process cohort: latest local checkpoint

The [individual flag review](../rust/parity/measurements/native-flag-configuration-keys-summary.json)
accepts only `AUTOROUTER_STATUSLINE` and `AUTOROUTER_DEBUG`. Fourteen original
validation cases, five raw-document controls and actual launcher, privacy and
saved-preference assertions support the review. Effective flag state comes from
those consumers and explicit report/branch composition; matching `readConfig`
results do not themselves contain either flag. Unexecuted raw type, command and
provenance permutations remain explicit. All eight relevant semantic source
files match the historical consumer execution. Seven configuration keys are now
covered and 20 remain pending. `ENABLE_TOOL_SEARCH` remains pending for a separate
raw-object versus typed-OS-environment boundary review.

The [owned-process cohort checkpoint](../rust/parity/measurements/native-owned-eof-cohort-summary.json)
executes the original 37-test cohort with temporary, bounded diagnostics. The
first run records 36 passes and one failure because the sandbox prevents the
resource-collector test from spawning `/bin/ps`. That failure is retained. One
explicitly declared permissions correction runs the identical executable and
source with process inspection permitted: all 37 tests pass. An initial missing
trait import in the temporary diagnostic is also retained as a compile failure.

The original timeout test passes in both cohort runs: both empty pipes reach EOF
on the first original tail round, no read is interrupted, and the emergency guard
does not fire and joins cleanly. The trace observes a leader fork notification
after TERM and before KILL. Notifications may coalesce and identify neither a
descendant nor a pipe writer; this does not establish the original failure's
cause or a fix. No deadline, shell workload or assertion predicate changes.
The two temporary source files are restored byte for byte, and all 695 ordinary
source inputs match the previous runtime-fixture snapshot.

All 588 definition rows, 53 limit rows and thirteen area objects remain unchanged.
The original full-workspace EOF failure and both transfer-size failures remain
unresolved; the overages remain 65,024 logical bytes and 39,936 expanded carrier
bytes. Fresh full-workspace/current-source CI, supported-platform, performance,
provider/task-quality, provenance, rollback, stabilization and cutover qualification
remain open. JavaScript 0.5.2 remains the shipping default; changes remain local.

## Owned-process pipe-tail ownership: latest local checkpoint

The [pipe-tail checkpoint](../rust/parity/measurements/native-owned-process-tail-summary.json)
corrects the cleanup order: the owned leader now remains unreaped through the
existing bounded pipe drain. Each drain round can safely repeat group KILL while
that leader reserves the group ID; final cleanup reaps afterward. The original
timeout, TERM grace, 64 drain rounds, per-read/aggregate byte limits, status fields
and four timeout assertions remain unchanged. A real-process regression drains
actual pipes, then uses an independent kernel wait to prove that the tail did
not consume the leader's exit status. This test covers the tail helper's contract;
the capture entrypoint's call order is separately source-reviewed.

Formatting, strict xtask/all-target Clippy and the 38-test audit cohort pass.
The workspace/all-target command records 1,382 passes, one source-cap failure and
ten ignored tests before Cargo stops at the failed xtask unit group. The three
remaining integration targets then pass eleven tests with one ignored helper;
both doc-test groups pass with zero tests. Composed evidence is 1,393 passes,
one failure and eleven ignored entries across 66 groups. The initial workspace
command remains failed; these are three separate commands. Both ordinary tool
binaries pass the original timeout test and the new ownership regression. The
historical EOF failure's cause remains unproven; current-source success does not
retroactively turn its retained failure into a pass.

Node checks, all 596 Node tests and the exact 58-file installed-package smoke
pass. Package evaluator timings are mock validation, not model measurements.

The correction and regression add 2,951 formatted source bytes. Exact offline
arithmetic now charges 33,622,407 logical bytes and 33,596,928 expanded carrier
bytes, exceeding the unchanged caps by 67,975 and 42,496 bytes respectively.
Actual logical-transfer admission fails before archive creation. Archive export,
restore, compressed checksums and release-feature executable bundles remain
unqualified. No source, fixture, vendor, license or schema omission is used to
fit the caps.

Coverage remains 539 covered/49 partial definitions, seven covered/20 pending
configuration keys, four covered/49 pending limits and thirteen partial areas.
Current-source CI, supported-platform, complete protocol/resource/performance,
provider/task-quality, provenance, rollback, stabilization and default cutover
gates remain open. Changes remain local; JavaScript 0.5.2 remains the shipping default.

## Ollama evaluator timeout: individual configuration acceptance

The [individual timeout review](../rust/parity/measurements/native-ollama-timeout-configuration-key-summary.json)
accepts only `AUTOROUTER_OLLAMA_TIMEOUT_MS`. Key-specific model defaults,
accepted bounds, malformed inputs and inactive-provider controls compose with
actual evaluator, shared-waiter, setup and doctor assertions. A zero timeout
disables the evaluator timer while retaining cancellation and response bounds.
Report and saved-preference behavior use explicit source composition alongside
the retained consumer tests; exhaustive value-by-provenance combinations are
not claimed as executed.

Independent review corrected one attribution: the empty local-timeout edit test
runs with the default Ollama evaluator active. A separate test covers an invalid
inactive Jev edit; inactive Ollama edit selection is reviewed source wiring.
The original review and its additive correction remain separately preserved.

Eight configuration keys are covered and nineteen remain pending. Jev timeout
parser boundaries and the raw-object versus OS-environment Tool Search boundary
remain pending. No original definition, limit or whole-area status changes.
All 695 native inputs and 111 frozen reference files retain their identities;
historical command results remain tied to their actual source snapshots. This
review adds no fresh workspace, CI, model-performance or release qualification.
The source-transfer overages and the remaining rollout gates remain open.

## Jev evaluator timeout: actual boundary comparison and acceptance

The [individual Jev timeout review](../rust/parity/measurements/native-jev-timeout-configuration-key-summary.json)
accepts only `AUTOROUTER_JEV_TIMEOUT_MS`. A newly declared synthetic experiment
executes 56 inputs through frozen JavaScript and both existing native parser
APIs. All complete configurations, literal serialized JSON, exact errors and
input kind/order/number-bit/UTF-16 observations match. JavaScript records 32
successful configurations and 24 configuration errors; the two native APIs make
112 calls. The previously missing minimum `1`, maximum `10000` and rejected
`10001` controls now have actual outcomes. Zero remains invalid for selected Jev;
the Ollama timer opt-out does not extend to this key.

Both child-free parser runs finish successfully within their declared bounds,
with complete output and unchanged source bindings. Two earlier compiler-wrapper
runs remain invalid because their post-exit process-group signals returned
`EPERM`, despite compiler exit zero, complete pipes and reaping. Exact compiler,
Cargo-library and produced-binary identities establish compilation provenance
only. The separately reviewed parser supervisor owns one nonspawning child and
signals its unreaped PID; this does not qualify compiler or Tool Search group
cleanup, or normalize arbitrary group-signaling errors.

Independent review composes the new parser outcomes with existing actual Jev
deadline/body cleanup, shared-work/cache and saved-edit evidence, plus explicit
report/provenance source wiring. Manually assigning zero to a public Jev config
field remains a separate embedding candidate, outside this accepted parsed-key
contract. These new controls do not increase the original-definition or workspace
test counts and are not evaluator-performance or provider measurements.

Nine configuration keys are covered and eighteen remain pending. All 588 original
definition rows, 53 limits and thirteen area statuses are unchanged. Tool Search
tests, the source-transfer size gates, current-source CI and the remaining
platform/performance/provider/rollout gates remain open. Native source inputs are
unchanged; JavaScript 0.5.2 remains the shipping default.

## Client profile: individual configuration acceptance

The [client-profile review](../rust/parity/measurements/native-client-profile-configuration-key-summary.json)
accepts only `AUTOROUTER_CLIENT_PROFILE`. Ninety-three specific retained
comparisons cover the three profiles, model compatibility, permission-argument
selection and Auto child-environment behavior. Five complete raw-document
controls and seventeen named consumer passes supply separate parser and actual
launcher/store/router evidence. Boolean, array, object and exhaustive report
permutations use explicit source composition where documented; they are not
newly executed cases.

The profile remains a nonsecret setting with environment/file/default provenance.
Neither implementation supports a profile policy lock. This acceptance preserves
Claude's permission decisions and makes no provider-account eligibility claim.
It also does not accept the separate raw-parent-object to typed-OS-environment
migration needed for Tool Search.

Ten configuration keys are covered and seventeen remain pending. No original
definition, limit or whole-area status changes. Historical commands retain their
actual source and exit identities; there is no new workspace or CI qualification.
Source-transfer size limits and the remaining release gates remain open.

## Keychain fixed output and timer limits

The [individual Keychain limits review](../rust/parity/measurements/native-keychain-fixed-limits-summary.json)
accepts only the shared 65,536-byte captured-output budget and the fixed
15,000 ms timer setting. Six retained Source subprocess controls and existing
native real-child tests exercise accepted/rejected edges on each pipe and their
combined output. Native production-clock tests remain pending at 14,999 ms and
reject at 15,000 ms for both a waiting child and blocked stdin. Actual named
passes exist in the historical focused runs and the current source checkpoint;
its unrelated source-cap failure remains a failed workspace command.

The output limit is a captured-byte budget, not an allocation, RSS or kernel-pipe
bound. The timer acceptance covers the Source option and native timer trigger,
not Source elapsed-time equivalence, total wall time including spawn/cleanup,
synchronous Drop, descendant cleanup or additional platforms. No real Keychain
account operations were performed for this evidence.

Six limits are covered and forty-seven remain pending. All configuration,
original-definition and whole-area statuses are unchanged by this two-row review.
Other response-size limits still need their own exact operational boundary
controls; similar constants and smaller custom limits do not substitute for them.

## Shared entropy formatting: current local checkpoint

The [entropy-sharing checkpoint](../rust/parity/measurements/native-entropy-hex-sharing-summary.json)
consolidates seventeen fixed-width nonce/token formatting operations across
sixteen existing files. Runtime callers and the independent audit bootstrap keep
separate small helpers. Entropy sizes, RNG calls, caller-specific errors, private
paths, permissions, collision handling, cleanup and all assertions remain intact.
Some allocation/drop points move; no identical allocation schedule, injected
entropy-failure test or performance improvement is claimed.

Formatting and strict workspace/all-target Clippy pass. One no-fail-fast
workspace/all-target run records 1,393 passes, one logical source-cap failure and
eleven ignored tests across 64 groups, including all 59 default-feature
integration targets. The command remains failed. The minimal bootstrap separately
passes strict Clippy and all 29 tests; these are feature reruns rather than
additional unique tests. Both doc-test groups complete with zero tests. Node
checks, all 596 Node tests and the installed 58-file package smoke pass. Its
displayed evaluator timings are mocks, not model measurements.

The formatted change saves 1,406 logical source bytes. Exact offline arithmetic
charges 33,621,001 logical bytes and 33,595,904 expanded carrier bytes, still
exceeding the unchanged caps by 66,569 and 41,472 bytes respectively. The carrier
saves 1,024 bytes after padding. Actual logical admission fails before encoding;
archive export, restore, gzip/checksum and executable-bundle qualification remain
open. Fixtures, vendor sources, licenses, inventories, schemas and caps are intact.

All 695 current inputs bind the new source snapshot. Historical source files and
two mutable-target compilation libraries have retained byte-identical preimages;
older summaries keep their original identities. Separate review confirms the
affected client-profile and Keychain contracts compose with the nonce changes,
and their current consumers pass. No additional coverage row is promoted by the
refactor: ten configuration keys and six limits are covered, seventeen keys and
forty-seven limits remain pending, and all 588 definition rows and thirteen areas
retain their previous statuses. Current-source CI and the full remaining
platform/protocol/resource/performance/provider/provenance/rollout gates remain
open. JavaScript 0.5.2 remains the shipping default; changes remain local.

## Exact evaluator response-byte boundaries

The [individual response-limit review](../rust/parity/measurements/native-evaluator-response-limits-summary.json)
accepts only the 65,536-byte decision ceiling and 1,048,576-byte local-model
metadata ceiling. A predeclared frozen JavaScript probe and two new native tests
execute six matching cases: both evaluators accept decisions exactly at the
limit and reject one extra byte; Ollama accepts exact-limit metadata and then
infers, while oversized metadata prevents inference.

The valid JSON bodies use trailing ASCII spaces, 997-byte chunks and no length
hint. Oversized bodies remain open, so normal EOF cannot explain rejection.
Positive production timers and caller cancellation stay enabled; native paused
time remains unchanged. Each engine issues nine mock requests and releases nine
bodies, including all three stalled overflow bodies. Full classifier decisions
match, with conservative requested-Opus fallback on each rejection. Reader
unlocks/cancellation and native body drops remain distinct adapter observations.

Both supervised runs exit zero with complete output, reaping, no supervision
errors and unchanged 695 native/111 frozen inputs. Formatting and focused strict
runtime Clippy pass; the existing vendored warning is retained. The native run
selects exactly two passing tests, with 529 filtered out. Thirteen specific
historical reader/decoder/deadline/cancellation passes compose through unchanged
production bytes; this is not a fresh full-workspace or real-provider result.

Eight limits are covered and forty-five remain pending. All definition,
configuration and area statuses are unchanged. The necessary tests add 6,927
formatted source bytes: current arithmetic charges 33,627,928 logical bytes and
33,602,560 expanded carrier bytes, exceeding the unchanged caps by 73,496 and
48,128 bytes. No export/restore or release fit is claimed. Earlier failed and
successful receipts retain their original source identities. Fresh CI and the
remaining platform/protocol/resource/performance/provider/provenance/rollout gates
remain open; JavaScript 0.5.2 remains the shipping default.

## Four fixed cache defaults

The [individual cache-limit review](../rust/parity/measurements/native-cache-fixed-limits-summary.json)
accepts classifier capacity 1,000, token-count capacity 100, and their 300,000 ms
TTLs. Independent reanalysis compares all nine retained Source/native controls:
2,126 complete wire calls and 3,248 ordered steps, including 1,007 actual
completion records. Full residence, touched-entry eviction, exact expiry and
zero-option controls match. Hits change LRU order without extending expiry;
active human-task/tool ownership survives cache pressure and expiry.

All five unchanged native tests also pass in the retained c45 workspace against
the current cache implementation. Its overall source-cap failure remains.
Full wire transcripts are historical evidence, not a fresh current-source
capture. Private native sizes, allocator/RSS and discontinuous wall-clock or
arbitrary-duration domains are not fabricated as equivalent observations.
Only four limit rows change: twelve are covered and forty-one remain pending.

## Secret-store configuration acceptance

The [individual secret-store review](../rust/parity/measurements/native-secret-store-configuration-key-summary.json)
accepts only `AUTOROUTER_SECRET_STORE`. Exact saved `file`/`keychain` membership,
missing-value defaults, locked reads, edits/migrations, no-secret history,
private reports and setup defaults compose with fifteen existing native passes
and ten relevant Source definitions. Other invalid-string classes and broader
contradictory-environment/backend/platform matrices remain explicit source
composition rather than new execution.

Runtime loading/reporting takes the selector from saved values before environment
merging. Fresh or replacement setup separately imports nonsecret environment
settings, including the store, before explicit flags and the implicit macOS
default. Existing merged setup preserves its saved selector unless the flag
changes it. Independent review caught an overly broad v1 environment claim;
the original and additive correction remain retained.

Eleven configuration keys are covered and sixteen remain pending. Neither review
changes the 539 covered/49 partial original definitions or thirteen partial
areas. Recent Node evidence is a retained current-JavaScript run plus separately
verified frozen-source equality; no new frozen probe, real Keychain operation,
complete migration or platform qualification is claimed. Native source remains
71dc: transfer arithmetic is still 73,496 logical/48,128 carrier bytes over the
unchanged caps. Fresh CI and the full remaining rewrite gates stay open.

## Tool Search environment boundary

The [individual Tool Search review](../rust/parity/measurements/native-tool-search-configuration-key-summary.json)
accepts only `ENABLE_TOOL_SEARCH`, with an explicit typed OS-string API migration
documented in the [embedding guide](../rust/docs/embedding.md). Absence defaults
to the string `"true"` in every profile; explicit strings, including empty,
remain unchanged. Eighteen retained original string/presence comparisons compose
with eighteen new raw JavaScript observations, seventeen actual child-process
pairs and one passing native private-report test containing nine rows in two
validation modes. Saved strings and report provenance remain preserved.

Six raw JavaScript inputs retain their original null, boolean, number, array or
object representations before Node materializes the child environment. Rust's
existing parent map accepts OS strings. Raw null reports as `"true"` but reaches
the child as `"null"`; the typed string `"null"` reports and launches as itself.
One non-coercible JavaScript object raises a pre-spawn `TypeError` and remains
explicitly unpaired. No unsupported raw native input or equivalent rejection
is invented. These controls do not prove Claude's deferred tool execution or
real organization-policy admission.

Root and independent reviews reconstruct all seventeen pairs, six differences,
the complete ordered bridge and four Source report contexts. Every supervised
stage exits zero with complete output, reaping and no recorded supervision
errors. Only the Source/native joined-child runs have zero signal attempts;
the separately scoped private and artifact processes retain their prereap
direct-PID attempts. No generic descendant or cross-crash guarantee is inferred.

Twelve configuration keys are covered and fifteen remain pending. The 539/49
original-definition counts, twelve accepted limits and thirteen partial areas
are unchanged. The required report regression adds 4,846 formatted source bytes;
current transfer arithmetic is 33,632,774 logical and 33,607,680 expanded carrier
bytes, over the unchanged caps by 78,342 and 53,248 bytes. The embedding note and
acceptance metadata are separately pinned outside the unchanged transfer
allowlist. Native inputs remain snapshot `105a`; no archive fit or fresh CI is
claimed. JavaScript 0.5.2 remains the shipping default and all remaining rewrite
gates stay open.

## Shared JSON contract comparisons

The [JSON comparison refactor](../rust/parity/measurements/native-json-comparison-sharing-summary.json)
shares recursive contract/report comparison across fifteen existing source files.
Each caller retains its explicit numeric policy: source double projection,
finite doubles, or guarded integer/float round trips. Function names, visibility,
all 510 existing assertion sequences and all workloads remain. Two new controls
check the intentional policy differences at large integers and preserve nested
keys, array positions and primitive kinds. The private source macro adds no
runtime API, dependency or feature.

On source snapshot `c4def`, formatting and strict workspace Clippy pass, with the
existing vendored warning retained. The primary workspace records 1,387 passes,
one size-cap failure and ten ignored tests, then stops. Three deferred xtask
integration groups separately pass eleven tests with one ignored fixture on the
same 696 inputs. Together these are 1,398 passes, one failure and eleven ignored
tests across 64 groups. A separate no-default-feature bootstrap passes 29 tests;
it is not added again to that aggregate. The initial sandbox run with local
listener denials remains retained as failed execution; the accepted HTTP tests
run with authorized loopback access.

The refactor saves 5,036 formatted file bytes, or 5,004 logical bytes after the
new manifest entry, and 5,120 expanded carrier bytes. Census eight charges every
new source/header/name/digest and confirms 33,627,770 logical and 33,602,560
carrier bytes: still 73,338 and 48,128 bytes over the unchanged limits. All fixed
fixtures, vendor sources, licenses, inventory rules and schemas remain intact.
This is a reduction, not archive-fit or CI approval, and promotes no coverage
row by itself.

## Four model-name configuration keys

The [individual model-name review](../rust/parity/measurements/native-model-name-configuration-keys-summary.json)
accepts the Haiku, Sonnet, Opus and Jev model settings. Seventy-one selected
original finite inputs and four additional default cases bind to a retained
same-hash differential report. Twenty raw JSON-text outcomes and four isolated
Jev backend controls remain independently inspectable. Thirteen named native
consumer tests also pass in the current run. The finite response streams are
not retained individually; the complete report and exact case membership are
the credited evidence.

Root and independent review close the exact blank/control predicates and report
mapping through explicit source composition. The earlier twelve proposed parser
controls and extra positive report permutations were not executed. The documented
native API keeps owned UTF-16 identities alongside String display projections;
explicit replacement clears overrides, and ordinary Value observation can fail.
Earlier failed Value-observer evidence remains failed. These settings have
environment/file/default provenance: organization policy does not admit model
locks. The independent review corrects broader generic policy wording in the
author's proposal without adding a policy feature.

Sixteen configuration keys are covered and eleven remain pending. Original
definitions remain 539 covered/49 partial, limits remain twelve covered/forty-one
pending and all thirteen areas remain partial. No provider availability, model
quality, price, account eligibility, platform or release approval follows from
model-name syntax and identity preservation. Source `c4def` and its failed size
gate remain unchanged; JavaScript 0.5.2 remains the shipping default.

## Two savings map capacities

The [individual savings-limit review](../rust/parity/measurements/native-savings-capacity-limits-summary.json)
accepts only the 1,000 in-flight request and 100 session ceilings. Current native
tests prove no early eviction at 1,000/100 and the expected eviction at
1,001/101. Complete original schedules replay 1,127 updates and nine full
snapshots, including ignored late work and a returning session marked partial.
All six capacity tests pass, including forged and reordered negative controls.

The Source side combines actual retained original callback passes, immutable
complete fixtures and reviewed unchanged source. Eight older execution receipts
are absent and receive no fresh-read credit; this is not a new Source snapshot
capture. Root and an independent reviewer preserve those limits explicitly.

Fourteen fixed limits are covered and thirty-nine remain pending. The separate
10,000-entry savings history limit, physical memory, transport cancellation and
overall resource qualification remain open. Configuration counts remain sixteen
covered/eleven pending, and all original-definition and area statuses remain
unchanged. Source `c4def` still fails the transfer-size gate; no commit, CI or
release approval follows from these two logical map bounds.

## Numeric conversion order correction

The [numeric parser checkpoint](../rust/parity/measurements/native-numeric-conversion-order-summary.json)
fixes a confirmed error-order difference in four numeric settings. JavaScript
converts the raw value before its numeric type check. A plain JSON object with
an own noncallable `toString`, directly or inside an array, can therefore throw
before the setting-specific validation error. Nine of twenty-five declared
controls differ before the correction; all twenty-five common response
envelopes agree afterward. The Source observations run once and are reused
without changing their frozen inputs.

The correction keeps numeric/string fast paths, type rejection, evaluator
masking and earlier errors. Stop-hook and Ollama timeout type guards still run
first. One native regression checks both Value and owned-document inputs,
including unchanged input data. The current core suite passes 242 tests with
two ignored; formatting and strict workspace Clippy pass, retaining the existing
vendored warning. These focused checks do not update the earlier full-workspace
checkpoint to the new source.

Snapshot `f7c82` changes only configuration parsing and that regression. Census
nine charges 33,631,576 logical and 33,606,144 expanded carrier bytes, exceeding
the unchanged caps by 77,144 and 51,712 bytes. This is offline arithmetic, not
archive/export/restore success. No fixtures, sources, licenses, workload,
allowlist or representation are removed to obtain a passing size result.

Exact extreme-depth JavaScript stack-limit errors remain unqualified; the
native conversion is iterative. Rejected nonprimitive inputs can allocate a
temporary string, and no performance improvement is claimed. A separate static
audit found that the async contributor-tool runner can signal a saved process
group after reaping its leader; that ownership repair remains open and is not
on these pure configuration-probe paths.

All coverage counts remain unchanged: 539/49 original definitions, sixteen/eleven
configuration keys, fourteen/thirty-nine fixed limits, and thirteen partial
areas. Minimum-confidence acceptance, current full CI, platform, performance,
live-task and cutover gates remain open. JavaScript 0.5.2 remains the shipping
default; no commit, push or release is approved by this checkpoint.

## Stop-hook cap configuration key

The [individual cap review](../rust/parity/measurements/native-stop-hook-configuration-key-summary.json)
accepts parsed `CLAUDE_CODE_STOP_HOOK_BLOCK_CAP` within the explicitly documented
native input domain. Thirty-six finite parser cases, an absent default and
fifty-four child-environment cases bind to the retained same-hash comparison.
Separate actual positive/negative Infinity controls, five historical consumer
passes and the current type-first regression supplement that evidence. Individual
finite response streams are not retained; no new product tests run for this
documentation and ledger decision.

The embedding guide now records the separately reviewed Stop-cap NaN input
migration. Native JSON cannot admit NaN; invalid-token rejection is not execution
of the cap guard with NaN. A future API admitting NaN requires a direct guard
regression. Parsed absence and zero remain distinct, explicit parent strings win,
and direct public-field mutation is not revalidated. Raw JavaScript parent
objects remain separate from the native OS-string interface.

Seventeen configuration keys are covered and ten remain pending. All original
definition, fixed-limit and area counts remain unchanged, including partial
original config definition six. Source `f7c82`, its failed size gate, the open
tool-child repair and all platform/performance/live/cutover gates remain.
JavaScript 0.5.2 remains the shipping default.


## Async tool subprocess ownership checkpoint

The [ownership and reader checkpoint](../rust/parity/measurements/native-tool-process-ownership-summary.json)
repairs a contributor-tool ordering defect: the old async runner could signal a
saved process group after a consuming wait had reaped its leader. The new owner
retains the unreaped reservation through group cleanup and retires its signal
lease before consuming the child status. Nonconsuming exit observation permits
same-group descendant pipe tails to finish under existing stop bounds.

Four actual regressions cover the production finalization seam, a consuming-wait
negative control, delayed descendant tails, cancellation after leader exit and
Drop before/after exit. Existing tool tests retain their bodies. An initial
unchanged deadline test failed because factual SIGTERM was cleared; the repair
now preserves failure signal facts while clearing an unqualified exit code.
That failed run and a later test-helper Clippy failure remain retained.

Bounded optional diagnostics preserve raw signal errors. A narrowly normal task
exit can retain its factual code while cleanup remains explicitly unconfirmed.
The strict report readers accept older reports and keep functional task quality
separate from cleanup. Three new reader tests reject unknown, oversized and
inconsistent metadata, prevent correlated cleanup failures from passing relative
gates, and check privacy without retaining command payloads.

Snapshot `064a9a` passes formatting and strict workspace Clippy, retaining the
existing vendored warning. The primary workspace records 1,395 passes, one
logical source-transfer cap failure and ten ignored tests. Three deferred default
integration groups separately pass eleven tests with one ignored on the same
696 native and 111 frozen inputs. Together these are 1,406 Cargo test passes,
one failure and eleven ignored across 64 groups; repeated shared-module tests
are included, and this is not a count of unique contracts. Ignored controls,
feature-gated release bundle tests and unreached doc tests receive no execution
credit from this checkpoint.

Census eleven computes 33,672,160 logical and 33,647,104 expanded carrier bytes,
117,728 and 92,672 bytes over the unchanged caps. Actual workspace execution
rejects the logical charge. No archive/export/restore fit is claimed and no
source, fixture, vendor, license, workload, representation or cap is omitted or
changed to obtain approval.

Sole-reaper assumptions, Tokio's best-effort Drop reaping and failure of both
kill mechanisms still limit cleanup guarantees. Older stdout-only command
readers do not prove exhaustive process-group cleanup. This macOS synthetic
checkpoint promotes no definition, key, limit or area and does not qualify Linux,
minimum platforms, TLS, performance or provider tasks. Fresh CI, size correction
and all remaining cutover gates are open; JavaScript 0.5.2 remains the shipping
default.


## Minimum-confidence configuration key

The [individual confidence-setting review](../rust/parity/measurements/native-min-confidence-configuration-key-summary.json)
accepts parsed `AUTOROUTER_MIN_CONFIDENCE` with a separately documented native
embedding migration. All eight predeclared depth-64/depth-256 controls produce
byte-identical complete response envelopes through the frozen JavaScript and
native document APIs. They preserve inputs and distinguish ordinary type errors,
primitive-conversion errors, opaque object properties and inactive-backend
masking. The deep environment remains a raw string until `JsDocument` admission;
no recursive Value or bounded input-observer failure substitutes for the parser.

Both fixed processes exit successfully with complete EOF and reaping, and zero
recorded supervision errors. Their receipts retain three pre-wait direct-PID
SIGKILL attempts each; this is not a zero-signal or general descendant-cleanup
claim. Cargo reuses its pinned cached core/serde outputs; only the artifact-only
probe is newly compiled. No release reproduction claim follows.

Earlier finite, isolated-backend, raw-Infinity and repaired conversion controls
remain separately scoped. Nine actual consumer passes bind to the unchanged
current workspace log. Threshold equality, unexecuted endpoint variants and
MIN-specific report source-label combinations retain their disclosed static
composition; they are not additional executed tests.

The embedding guide explicitly distinguishes parsed JSON from direct mutation
of the public `f64`, which can contain NaN. JSON rejection is not numeric-guard
execution with NaN. The reviewed resource migration permits host exhaustion
diagnostics to differ for rejected nonnumeric inputs while retaining ordinary
conversion/type/range precedence and validity. No supported-input depth ceiling
or universal V8 stack threshold is introduced; extreme-depth exhaustion is not
executed or qualified by the eight bounded controls.

Eighteen configuration keys are covered and nine remain pending. Original
definitions stay 539 covered/49 partial, fixed limits stay fourteen/thirty-nine,
and all thirteen areas remain partial. The 696 native source inputs are
unchanged, so the existing 1,406-pass/one-size-failure/eleven-ignored checkpoint
and census eleven keep their original scope. No full suites rerun solely for
this metadata and documentation acceptance.

Two independently drafted source-sharing candidates were left unapplied: the
fixture macro saves only 723 raw bytes and the paired benchmark plumbing only
395 before formatting. Their complete drafts contradict the larger preliminary
estimates. They do not solve the 117,728-byte logical or 92,672-byte expanded
overage. The caps, fixtures, vendors, workloads and source representation remain
unchanged. Fresh CI and all remaining platform, performance, task and cutover
gates are open; JavaScript 0.5.2 remains the shipping default.


## Release-error privacy and two request/turn limits

The [release-error checkpoint](../rust/parity/measurements/native-release-error-privacy-summary.json)
corrects an actual output-boundary defect. `ReleaseError` has open JSON detail
and a caller-supplied static code; its type did not guarantee safe reasons.
Verification previously copied arbitrary reason/status values into callbacks,
events and persisted reports. Its CLI error wrapper also copied arbitrary fields,
allowing state/phase/code overrides and panics on nonobject detail.

Both boundaries now retain only recognized code/reason pairs and documented
metadata. Unknown codes become `verification_error`; malformed reasons fall
back to a known code. Integer HTTP statuses remain valid through 999. Attempt
reports never acquire error-supplied `current_latest`; CLI preflight may retain
null or a strict version string without treating parseability as provenance.
Cancellation precedence, availability versus failure, retry/deadline bounds and
the historical checksum-error report shape remain intact. The native API remains
`Result<Value, ReleaseError>`; successful custom installer results, artifact
metadata and direct caller error formatting remain trusted inputs. This policy
is stricter than Source forwarding arbitrary injected `ReleaseError.reason`.

The actual tests-first run passed six existing controls and failed all four new
privacy regressions. After repair, all eleven focused tests pass, including
three cancellation schedules. The exact unchanged Source original nine passes
once with its ordinary private `Error.message` injection. The [five-site
supplement](../rust/parity/assertion-mappings/release-error-privacy-contracts.json)
accepts only that original definition after explicit typed-error API review.
Its former known-safe-error assumption is retained historically and corrected;
all other 587 definitions and the original release mapping remain unchanged.
Coverage is now 540 covered and 48 partial definitions; all thirteen areas remain
partial.

The current source snapshot has 696 native inputs and all 111 frozen files
verified before and after each check. The main workspace command records
1,400 passes, one source-transfer cap failure and ten ignored tests over 61
completed groups. Three deferred xtask integration targets separately pass
eleven tests with one ignored control. Their union is 1,411 passes, one failure
and eleven ignored tests over 64 groups, not a green workspace or doc-test
qualification. Formatting and strict workspace/all-target Clippy pass, with the
previous vendored `hyper-util` unused-timer warning retained.

JavaScript static checks and exact-package smoke also pass. The first test run
inherited the artifact runner's 0077 umask and failed one existing test that
creates a 0644 target without chmod. Changing only the child umask to 0022 makes
all 596 unchanged tests pass; the initial 595-pass/one-failure result remains
retained. All 36 shipping modules are unchanged. Package smoke verifies offline
installation and lifecycle behavior from the actual 58-file archive.

The separately reviewed [request/turn limit increment](../rust/parity/measurements/native-request-turn-fixed-limits-summary.json)
accepts only the default 33,554,432-byte request ceiling and eight retained keys
per turn record. Native count requests pass at exactly 32 MiB with forwarded
length/hash equality; one extra streamed chunked byte receives 413 without
count forwarding or evaluation. Fresh Source evidence uses the original
configured 1000/1001-byte inference rejection; exact parsed default captures and
the common pre-route byte loops provide the remaining composition. This is not
a fresh Source 32 MiB run, exact-limit inference execution or a RAM/RSS bound.

Alias acceptance composes the actual native 100-discovery default-state test,
the retained 217-case Source/native differential containing a 13-key overflow,
and unchanged current alias code. Each record retains its primary plus seven
secondary keys in insertion order; an existing duplicate does not refresh its
position. Eight is not the global alias-map bound. Default 1000-record/attempt
capacities and the 30-minute retired TTL remain pending boundary execution.
Sixteen fixed limits are covered and 37 pending; configuration keys remain
18 covered and nine pending. No whole area is promoted by these increments.

Census twelve computes 33,693,363 logical bytes and 33,668,096 expanded carrier
bytes: 138,931 and 113,664 bytes above the unchanged 33,554,432-byte caps.
The actual source-transfer test rejects the logical sum before archive encoding;
the census is exact offline arithmetic, without export/restore/gzip success.
The privacy repair and regressions add 21,203 source bytes. All source, fixture,
license, workload and vendor content remains intact. Current-source CI,
platform/TLS/resource/performance/live/release/rollback/stabilization and cutover
gates remain open. JavaScript 0.5.2 remains the shipping default; no commit, push,
version, tag, publication or user-configuration action was performed.

## Turn-state metadata and continuity repair

The [pin repair checkpoint](../rust/parity/measurements/native-turn-state-pin-repair-summary.json) fixes a reproduced loss of confirmed tool ownership when a pin starts as a primitive or array. Committed metadata now preserves JavaScript object spread, including UTF-16 code-unit string properties. An immutable exact document accessor retains generated lone surrogates, while the Value observation remains explicitly lossy. Private confirmation controls both tool-owner lookup and router continuity labels. Raw Value selection retention and typed native tool identities keep their separate contracts.

Four new regressions first failed against the old implementation. All eight new regressions pass in the full current-source run. Fourteen actual Source/native scenarios now produce byte-identical complete results across 4,176 observations per engine, including the original eleven differing observations, both independent 1,000-entry ceilings, pending-attempt expiry protection and exact Unicode ownership. Independent review accepts only the two fixed-limit rows: 18 covered and 35 pending. The original definition ledger remains 540 covered/48 partial; configuration stays 18 covered/9 pending and all thirteen areas remain partial.

Formatting, strict workspace/all-target Clippy, all 596 Node tests and the installed 58-file shipping package pass. The Rust checkpoint has 1,419 passes, one failure and eleven ignored entries across 64 groups in two commands; these are execution counts rather than unique tests. Initial sandbox-denied mock-listener failures remain retained separately from the successful loopback-enabled retries.

The unchanged source-transfer gate still fails before encoding: complete logical payload is 33,710,273 bytes, 155,841 above its 32 MiB cap. Exact offline carrier arithmetic is 33,684,992 bytes, 130,560 above its separate cap. No successful export/restore, current performance or memory result, supported-platform qualification, release or cutover is inferred. JavaScript 0.5.2 remains the shipping default.


## Token-count response ceiling

The [individual count-response limit evidence](../rust/parity/measurements/native-count-response-fixed-limit-summary.json) accepts only the default 65,536 decoded-byte ceiling. Six actual frozen Source/current Rust controls match every complete expected row: valid JSON at 65,535 and 65,536 bytes succeeds, while 65,537 bytes returns an unknown count without caching it. Plain and gzip responses exercise the same decoded sizes; later successful retries are cached. Internal Source abort signals and native per-call clock checks exclude the unchanged 1,500 ms deadline as the rejection cause.

Two new native tests preserve the entire previous token-counter file byte-for-byte when removed. All 20 focused tests, formatting and strict workspace/all-target Clippy pass on the final source. This is test-only work and does not replace the previous full-workspace checkpoint or qualify CI, performance, platforms or release artifacts. The process receipts retain their three prewait signal attempts per stage, complete output/EOF and successful reaping; they make no general descendant or cross-crash guarantee.

The fixed-limit ledger is now 19 covered and 34 pending. Definitions remain 540 covered/48 partial, configuration remains 18 covered/9 pending and all thirteen areas remain partial. The unchanged development source-bundle caps still fail: the latest offline census is 170,525 logical bytes and 145,408 expanded-carrier bytes above 32 MiB. It is arithmetic evidence, not a successful export or restore; the proposed development-only 40 MiB change remains unapplied pending user approval. No commit, push, CI, release or shipping-default switch was performed in this increment.


## Consuming bounded-reader API migration

The [per-assertion supplement](../rust/parity/assertion-mappings/bounded-json-native-api-migrations.json) and [focused evidence](../rust/parity/measurements/native-bounded-json-api-migration-summary.json) accept only original bounded-JSON definitions 2–5. The unchanged frozen Source file passes all five tests and the six native integration tests pass, including the corresponding overflow, malformed/empty response, pending cancellation and pre-cancelled overflow schedules. The review maps twelve static assertion sites; structural loop expansion is not an instrumented execution count.

The [embedding guide](../rust/docs/embedding.md) now documents the accepted native interface: the three consuming functions return typed `ReadError` values and release an owned body. These outcomes and destruction checks replace JavaScript Error identity, reader-lock and unresolved cancellation-Promise observations. The borrowed incremental `DecodedResponseStream::read` API has a separate lifetime and is excluded from the release-before-return guarantee. Historical mappings, complete original row records and assertion hashes remain retained.

Only those four definition rows changed, taking the ledger to 544 covered and 44 partial. Configuration remains 18 covered/9 pending, fixed limits remain 19 covered/34 pending and all thirteen areas remain partial. The fresh ordinary check owner verifies 696 native and 111 frozen inputs before and after each command but has no finite outer wall/output bound. This review adds no runtime behavior, platform/performance approval, full-workspace result or release qualification.


## Buffered gateway pipeline ownership

The [pipeline evidence](../rust/parity/measurements/native-buffered-gateway-pipeline-summary.json) adds twelve actual native gateway controls over HTTP, TLS 1.2 and TLS 1.3. In each case B becomes ready upstream while A's headers are held. The writer independently holds both final flushes; A confirms only after its flush succeeds. Cancelling A's actual request token after retirement and sending stale notifications leaves B unchanged. Each protocol then covers B success, an actual B `BrokenPipe` at final flush, cancellation of B's actual request token, and crossing B's live configured deadline. Failure leaves B's previous tool owner intact while preserving A's confirmed result.

All 21 flush-module tests pass: twelve new pipeline controls and nine existing tests, with the original entire test file retained as an unchanged prefix. Each successful schedule checks exact request/response payloads and framing, outcome/callback counts, model/tool ownership, and the fixture's zero-live resource counters after cleanup. The deadline controls advance Tokio time by 30,001 ms against a configured 30,000 ms timer after provider I/O has ended, under a predeclared 60-second virtual watchdog. They do not establish exact timer equality or wall-clock performance. A failed secondary abort-join timeout can still leave a handle unjoined on an already failed test path; no universal exceptional cleanup guarantee is claimed.

The fresh broader checkpoint records 1,433 passes, one failure and eleven ignored tests across 64 Rust result groups in two commands. Cargo stopped after the unchanged logical source-transfer cap failed; the three remaining integration targets were then run explicitly and pass. Formatting and strict workspace/all-target Clippy pass. Required Node checks pass, including all 596 JavaScript tests and the unchanged JavaScript 0.5.2 package's 58-file archive, offline installation and mock lifecycle checks. These are execution counts, not a unique-test count or native release-artifact qualification.

The actual source-transfer failure remains open. Offline census reports 33,765,631 logical bytes (211,199 above the unchanged 32 MiB cap) and 33,740,288 expanded-carrier bytes (185,856 above its unchanged cap). No export/restore success, cap increase or artifact omission is claimed. The 40 MiB development-only proposal remains awaiting user approval. This increment supplies experimental buffered-constructor evidence; production transport/TLS promotion, remaining protocol/resource/platform/performance/release gates and default cutover remain open. Definitions stay 544 covered/44 partial, configuration 18 covered/9 pending, fixed limits 19 covered/34 pending and all thirteen areas partial.

## Buffered gateway first-flush failure checkpoint

The [three additional first-flush controls](../rust/parity/measurements/native-buffered-gateway-a-flush-failure-summary.json) fail the first response while a second completed upstream response is queued, over isolated HTTP, exact TLS 1.2 and exact TLS 1.3 fixtures. Both requests fail downstream delivery, neither confirms its selected model, prior confirmed owners survive, and the second response never reaches the wire. The complete focused module passes all 24 tests on source `45d6883`; formatting and strict workspace/all-target Clippy also pass. All 21 earlier test bodies remain unchanged. These synthetic experimental-buffered-backend checks do not approve production TLS, complete exceptional cleanup, platforms or performance.

The earlier broad 1,433-pass checkpoint belongs to source `303d833` and retains one source-transfer cap failure and eleven ignored tests. It is not a broad run on this newer test-only source. The offline census for that source is 215,391 logical bytes over the unchanged 32 MiB source-transfer cap; it is not a successful export/restore. The proposed development-only cap change remains unapproved and unapplied. Coverage remains 544 covered/44 partial definitions, 18/27 configuration keys and 19/53 fixed limits; JavaScript remains the shipping default.

The [local-token constructor comparison](../rust/parity/measurements/native-local-token-constructor-summary.json) executes the same 36 raw configuration profiles in each engine: 26 admissions agree and ten non-string admissions differ. The reviewed native API requires an explicit string local token; numeric-array authentication in JavaScript is acknowledged as a real difference. Exact raw configuration capture and separate child-environment conversion remain intact. A new native regression passes captured-value rejection, explicit sixteen-UTF-16-unit string replacement, and clearing without resurrection in both auth modes. It changes no production behavior. Compilation, formatting and strict Clippy pass on the new test-only source `2d00c83`; the constructor comparison belongs to the preceding `45d6883` source. No configuration key, baseline definition or fixed-limit row is promoted.

The current offline census is 218,330 logical bytes over the unchanged 32 MiB source-transfer cap. Fresh broad tests and CI, complete platform/TLS/resource and performance evidence, final artifacts and cutover remain open.

## Credential configuration checkpoint

The [provider-key review](../rust/parity/measurements/native-provider-credential-configuration-keys-summary.json) individually accepts `ANTHROPIC_API_KEY` and `TYPESAFE_API_KEY`, bringing configuration coverage to 20 of 27 keys. It composes retained named actual parser/consumer tests with exact unchanged credential code and the shared secret-report/store branches. Historical passing callbacks are credited individually despite their broader wrapper failure; this is not a new whole-run pass. Native API boundaries and the missing complete TokenCounter POST/native Jev socket matrices remain explicit. No original definition, fixed limit or whole area is promoted.

The [new CLI regression](../rust/parity/measurements/native-local-token-cli-setter-summary.json) passes the same eight commands against the fresh Rust CLI and frozen JavaScript CLI: fifteen-unit rejection without mutation, sixteen-unit ASCII and astral acceptance, hidden presence reporting with an empty environment override, and isolated unset. Both lanes execute the same test binary and assertions. Compilation, formatting and strict Clippy pass on test-only source `77da7f3`; no production behavior changes. These checks do not complete the local-token key audit or qualify full CLI/platform/resource behavior.

The unchanged source-transfer gate remains open: the current offline census is 221,903 logical bytes and 196,608 expanded-carrier bytes above 32 MiB. There is no current successful export/restore; the development-only 40 MiB proposal remains unapproved and unapplied. JavaScript remains the shipping default.

## Default history line-size checkpoint

The [default line-size review](../rust/parity/measurements/native-history-line-byte-default-limit-summary.json) accepts only `history_line_bytes = 16384`. The same integration test runs three private `sessions show` commands against each CLI: valid UTF-8 JSON lines of 16,383 and 16,384 bytes are returned, while the 16,385-byte line is skipped and its following sentinel survives. All six complete observations match, including coverage counters; configuration/log bytes, identities, modes and directory contents remain unchanged. All seven defaults stay in effect. Normalized records use the existing guarded JSON-number comparison while every field, type and record position is checked.

The first compile failed because the test included comparison macros in statement context. Its retained syntax correction compiles and passes both CLI lanes, formatting and strict Clippy on source `e10b364`. No production behavior changes. Coverage is now 20 fixed limits covered/33 pending, configuration 20/27 and original definitions 544 covered/44 partial; the other six history limits and all areas remain unapproved as wholes. This adds no complete CRLF/EOF/malformed/resource, platform or release qualification.

The current offline source census is 227,961 logical bytes and 202,752 expanded-carrier bytes above the unchanged 32 MiB cap. There is no current successful export/restore; the development-only 40 MiB proposal remains unapproved and unapplied. Historical broad tests and CI do not qualify this newer source, and JavaScript remains the shipping default.


## Default history record and line ceilings

The [record and line review](../rust/parity/measurements/native-history-record-line-default-limits-summary.json) accepts only the default 5,000-record and 10,000-line limits. The same two integration tests run eight private `sessions show` commands against each CLI. Both engines accept 4,999 and 5,000 records, cap 5,001, exclude invalid and foreign-session rows from record capacity, and count duplicate records before summary deduplication. Separate line controls retain the final valid record at lines 9,999 and 10,000 and exclude it at 10,001; only one or two records are admitted, so the record limit cannot mask the line limit.

All sixteen retained observations match the declared counters and expected results. Every returned record, all seven unchanged defaults and file/configuration bytes, identities, modes and entry inventories are checked. Four focused runs each pass one test with seven filtered out; compilation, formatting and strict workspace/all-target Clippy pass on test-only source `9eac327`. The existing 25,720-byte test file remains an exact prefix. The cached native CLI is unchanged; this is neither a fresh whole-workspace result nor current CI qualification.

Fixed-limit coverage is now 22 of 53, leaving 31 pending. Configuration remains 20 of 27 keys, definitions remain 544 covered/44 partial, and all thirteen areas remain partial. Complete CRLF/EOF/malformed-input, storage/resource, platform, performance and release checks remain separate. In particular, the guarded JSON-number value comparison in these tests does not approve the separate public CLI numeric spelling mismatch; that writer behavior remains under review.

The current offline census computes 33,791,120 logical bytes and 33,765,888 expanded-carrier bytes: 236,688 and 211,456 above the unchanged 32 MiB caps. A copied census script first rejected its obsolete source hash; the failure is retained, and its corrected hash-only version succeeds. No product export or restore was performed. The development-only 40 MiB proposal remains unapproved and unapplied. JavaScript 0.5.2 remains the shipping default; no commit, push, CI run, release or cutover was performed in this increment.


## History JSON number formatting repair

The [numeric writer evidence](../rust/parity/measurements/native-history-json-number-fidelity-summary.json) retains a demonstrated CLI compatibility failure: the old Rust writer produced 22 mismatches across twelve inputs and both `sessions show --json` and `sessions list --json`. For example, schema version `2.0` was rejected by a strict integer consumer that accepts Source's `2`, while latency values used `12.0` or exponent notation where JavaScript writes `12` or fixed decimal notation. The unchanged regression passed against the frozen JavaScript CLI before the repair.

The two Rust history JSON branches now use a private formatter that writes floating-point numbers with the existing RyuJS implementation and delegates the previous pretty-printer's nine structural hooks. Exact native integer variants remain unchanged. The same twelve-input regression passes against the fixed Rust CLI and frozen Source with zero mismatches. All six sessions unit tests pass, including two new tests containing seventeen float controls, six exact-integer controls and nested formatting/string checks. All three earlier default history-boundary tests also pass. The old failure, both source snapshots, actual compiler records and exact old/new executables remain retained; the test expectations were not relaxed.

Compilation, formatting and strict workspace/all-target Clippy pass on source `9e5b39b`. This is a focused correctness repair, with no baseline definition, configuration key, fixed limit or whole-area promotion. Coverage remains 544 covered/44 partial definitions, 20/27 configuration keys, 22/53 fixed limits and thirteen partial areas. Local diagnostic JSON formatting, file-size metadata precision above 2^53 and broader CLI representation requirements remain separate open work. Existing process-owner exceptional cleanup limits remain explicit, and no full-workspace, current CI, platform, performance, provider or release qualification follows.

The current offline census is 249,802 logical bytes and 224,768 expanded-carrier bytes above the unchanged 32 MiB source caps. It is not an actual export or restore. The development-only 40 MiB proposal remains unapproved and unapplied. JavaScript 0.5.2 remains the shipping default; this increment includes no commit, push, CI run, release or cutover.


## Diagnostic JSON numbers and shared CLI formatting

The [diagnostic output evidence](../rust/parity/measurements/native-doctor-json-number-fidelity-summary.json) retains four actual mismatches in the old Rust `doctor --evaluate-local --json` output. Agreement and minimum agreement were emitted as `1.0`, and the two under-routing rates as `0.0`, where JavaScript emits `1` and `0`. The fifth integer field was already correct. All synthetic diagnostic/model/endpoint/configuration assertions passed before the final numeric check failed. The same unchanged regression passed against frozen JavaScript.

A private shared CLI formatter now supports compact diagnostic JSON and pretty history JSON with the same existing RyuJS float policy. Exact native integers stay unchanged. Both diagnostic JSON branches use it; the history wrapper and its existing golden tests retain their behavior. The fixed Rust and frozen JavaScript diagnostic runs report zero mismatches. The existing configuration-error test, six sessions unit tests, one new compact-formatting test and the unchanged twelve-input history CLI regression pass. Formatting and strict workspace/all-target Clippy pass on source `27259d3`. These are synthetic loopback and output checks, without an installed model or real-provider inference.

The source inventory retains all 696 old paths and adds the formatter module as the 697th path under the existing admission rule. The diagnostic integration executable is byte-identical across old-native, Source and fixed-native runs; the CLI and unit executable were rebuilt, with their exact identities and original failure retained. No full-workspace/current-CI, platform, model-quality, performance, cancellation/resource, release or cutover result is inferred. Coverage remains byte-for-byte unchanged: 544 covered/44 partial definitions, 20/27 configuration keys, 22/53 fixed limits and thirteen partial areas. Other report writers and large file-size metadata precision remain open.

The current offline census is 33,810,184 logical bytes and 33,785,344 expanded-carrier bytes, exceeding the unchanged 32 MiB caps by 255,752 and 230,912 bytes. It is arithmetic evidence, not a successful export or restore. The development-only 40 MiB proposal remains unapproved and unapplied. JavaScript 0.5.2 remains the shipping default; no commit, push, CI run, release or cutover was performed.


## History file-size precision at the CLI boundary

The [file-size precision evidence](../rust/parity/measurements/native-history-file-bytes-precision-summary.json) covers ten declared metadata sizes with one 224-byte synthetic log. Frozen JavaScript's exported reader uses a fully mocked filesystem and emits twenty complete API reports plus twenty explicitly composed CLI JSON reports. Rust's pure parser and actual private enabled-report writer reproduce twelve full-report and raw-token differences across six sizes, including an exactly representable binary64 integer whose shortest decimal differs from its exact integer spelling. These are API/producer compositions, not actual huge-file CLI or operating-system stat measurements.

The enabled Rust history JSON producer now projects only the known per-file `coverage.file_bytes` fields above JavaScript's safe-integer range to binary64 before formatting. Native metadata, admission, read limits and returned API Values retain exact `u64` sizes; the generic writer and unrelated integer fields remain exact. This typed API distinction is explicitly documented in the embedding guide and appended to the history mapping without changing original assertion rows. All twenty complete outward reports and raw size tokens now match JavaScript. Nine sessions unit tests and four existing real-file CLI history regressions pass, as do formatting and strict workspace/all-target Clippy. The first test compile failed on macro recursion; its unchanged literal oracle was restructured without raising crate limits or changing expectations, and the failure is retained.

Source `24b126d` retains all 697 native inputs and all 111 frozen files. This increment promotes no coverage rows: definitions remain 544 covered/44 partial, configuration 20/27, fixed limits 22/53 and all thirteen areas partial. Full-workspace/current-CI, real-provider, platform, resource and performance qualification remain open. The offline census is 33,824,236 logical bytes and 33,799,168 expanded-carrier bytes, respectively 269,804 and 244,736 above the unchanged 32 MiB caps; no export/restore was performed. The development-only 40 MiB proposal remains unapproved and unapplied. JavaScript 0.5.2 remains the shipping default; no commit, push, release or cutover was performed.


## Default history per-file byte boundary

The [4 MiB file-limit evidence](../rust/parity/measurements/native-history-file-byte-default-limit-summary.json) uses real private regular files of 4,194,303, 4,194,304 and 4,194,305 bytes. All 257 physical lines are valid JSON and strictly below the line limit; record, line and total-byte limits cannot mask the per-file boundary. The same newly compiled integration executable runs three actual `sessions show --json` commands against each engine. Rust and frozen JavaScript both return 257 records below and at the ceiling, then 256 above it, where only the final sentinel LF is outside the read budget. All six retained observations match the declared input hashes and complete coverage fields, including truncated/incomplete-tail reporting.

The actual tests check every returned record value and position, summary counts/models, seven unchanged defaults, log/configuration bytes and file/directory identities and modes. Full CLI output is checked inside each test; only typed coverage/count/hash observations are separately retained. Both focused runs pass one test with nine filtered out; compilation, formatting and strict workspace/all-target Clippy pass on test-only source `98ec924`. The native CLI is cached and byte-identical to the preceding checkpoint. All old 42,788 test bytes remain an exact prefix, and the existing helper's stricter input precondition is unchanged.

Only `history_file_bytes = 4194304` is accepted, bringing fixed-limit coverage to 23 of 53 with 30 pending. Definitions remain 544 covered/44 partial, configuration 20/27, and all thirteen areas partial. This does not qualify total-byte/directory/file-count limits, full-reader concurrency/race/error behavior, exceptional cleanup, platforms, performance or release artifacts. The offline source census is 33,831,344 logical bytes and 33,806,336 expanded-carrier bytes, exceeding the unchanged 32 MiB caps by 276,912 and 251,904. No export/restore was performed; the development-only 40 MiB proposal remains unapproved and unapplied. JavaScript 0.5.2 remains the shipping default; this increment includes no commit, push, CI run, release or cutover.


## Default history aggregate byte boundary

The [16 MiB aggregate-limit evidence](../rust/parity/measurements/native-history-total-byte-default-limit-summary.json) uses five real private regular files, each strictly below 4 MiB, with total sizes one byte below, at and above 16 MiB. A fourth dataset adds a sixth recognized file after the five accepted files exactly exhaust the budget. All physical lines are valid and below 16 KiB; record, file and directory counts cannot mask the aggregate boundary. Both engines run four actual `sessions list --json` commands plus an independent `sessions show --json` of the final file in the above-cap dataset.

The eight complete list reports match the predeclared full oracles and their hashes, and both show reports match all 257 expected records. Below and at the ceiling, all five summaries remain complete and `byte_limit_reached` is false. One byte above, only the last summary is truncated with an incomplete tail; the aggregate is partial but `byte_limit_reached` stays false. The separate sixth-candidate case sets that flag and reports one skipped file while all five returned summaries remain complete. The inspected pre-open guard explains the distinction; this is not an open-syscall trace. Reading the last file alone returns its complete final record, confirming the aggregate-limit isolation.

Both focused runs pass one test with ten filtered out; compilation, formatting and strict workspace/all-target Clippy pass on test-only source `7b54ec4`. The same fresh integration executable exercises the cached, unchanged native CLI and frozen JavaScript. Every field of the full summaries, aggregate coverage and defaults is checked, together with all file/configuration bytes, identities, modes and inventories. Complete parsed CLI report values are retained; reserialization does not establish raw CLI token or property-order equality. All old 49,896 test bytes remain an exact prefix.

Only `history_total_bytes = 16777216` is newly accepted, bringing fixed-limit coverage to 24 of 53 with 29 pending. Definitions remain 544 covered/44 partial, configuration 20/27 and all thirteen areas partial. File-count/directory-entry, whole-reader, resource/exceptional cleanup, platform, performance and release qualification remain open. The offline census is 33,842,991 logical bytes and 33,818,112 expanded-carrier bytes, respectively 288,559 and 263,680 above the unchanged 32 MiB caps. There was no export/restore; the development-only 40 MiB proposal remains unapproved and unapplied. JavaScript 0.5.2 remains the shipping default; no commit, push, CI run, release or cutover occurred.


## Default history file selection and directory scan counts

The [file/directory-count evidence](../rust/parity/measurements/native-history-file-directory-default-limits-summary.json) executes six boundary cases through each actual CLI using the same newly compiled integration executable. Recognized logs contain one 215-byte valid row, isolating the 100-file selection default at 99, 100 and 101 candidates. Both engines return the highest 99/100/100 IDs in descending order; the above-cap case skips only ID 000 and reports partial coverage. All 299 full summaries per engine, defaults, ordering and aggregate fields match the predeclared oracles.

The directory test uses 9,999, 10,000 and 10,001 unrecognized empty files, isolating scanning from file selection and byte budgets without depending on directory order. Both engines return empty session lists and zero matching/skipped/unreadable files and bytes. Only the last case reports directory-scan truncation and partial coverage. The counter includes the 10,001st lookahead entry before filename filtering; this does not establish a strict filesystem syscall or buffered-dirent-read bound. Exact name-inventory hashes and all file/configuration bytes, identities, modes and inventories are checked before and after each command.

All twelve complete report values match. Four focused test runs each pass one test with twelve filtered out; compilation, formatting and strict workspace/all-target Clippy pass on test-only source `7f3847e`. All old 61,543 test bytes remain an exact prefix, with both reviewed inserts retained once. The native CLI remains cached and byte-identical to the preceding checkpoints; Node is only the frozen reference. Retained markers serialize parsed actual CLI values, so raw CLI token/property-order equality and exceptional cleanup are separate requirements.

Only `history_files = 100` and `directory_entries = 10000` are newly accepted, bringing fixed-limit coverage to 26 of 53 with 27 pending. All seven history defaults now have individual numerical-boundary acceptance; complete-reader, mixed-entry/race/error/resource/platform behavior remains unqualified. Definitions stay 544 covered/44 partial, configuration 20/27 and all thirteen areas partial. The offline source census is 33,855,816 logical bytes and 33,830,912 expanded-carrier bytes, 301,384 and 276,480 above the unchanged 32 MiB caps. No export/restore occurred; the development-only 40 MiB proposal remains unapproved and unapplied. JavaScript 0.5.2 remains the shipping default, with no commit, push, CI run, release or cutover.

## Settings overlay default file boundary

The [settings-file boundary supplement](../rust/parity/measurements/native-settings-file-byte-default-limit-summary.json) verifies the unchanged 2 MiB ceiling using actual private regular files at one byte below, at and one byte above the limit. Native and frozen JavaScript direct APIs accept the first two while preserving permission anchors, `defaultMode`, environment and opaque settings. The oversized file returns the fixed safe error and leaves the previous overlay intact. A fourth inline-string control confirms that this ceiling applies to file input; it does not test operating-system argument capacity.

All nine native settings tests pass, including the eight unchanged tests. The four frozen controls, formatting and strict workspace Clippy pass. Only `settings_file_bytes` is newly covered: fixed limits are 27 covered/26 pending; definitions remain 544 covered/44 partial and configuration remains 20 covered/7 pending. Native and Node renderer commands and private path anchors are checked against separate exact expectations. Stable-file evidence does not qualify stat/read races, the generated renderer, complete launcher behavior or other platforms. JavaScript 0.5.2 remains the shipping default; full CI, source-transfer capacity, performance, packaging, rollback and cutover remain open.

## Real-clock classifier failures and current workspace checkpoint

The [four-mode real-clock supplement](../rust/parity/measurements/native-router-concurrency-real-clock-summary.json) executes oversized, malformed, stalled and HTTP 503 Jev responses twice using the original 20 ms deadline and two-second schedule bound. The 13 native concurrency tests and all ten frozen JavaScript concurrency tests pass. All six original assertion sites and 35 captured observations are retained. Supplemental native checks verify caller independence, a successful retry after failure, cache insertion only after success, and normal awaited worker cleanup.

The [reviewed ownership migration](../rust/parity/assertion-mappings/router-concurrency-native-api-migrations.json) accepts only original definition #9: JavaScript cancellation-callback observation becomes owned-body abandonment in Rust, without claiming execution or identity of its never-settling Promise. The embedding guide states the contract and excludes blocking destructors, custom-transport background tasks and TCP/TLS teardown. Definitions are now 545 covered/43 partial; configuration remains 20 covered/7 pending and limits remain 27 covered/26 pending.

The complete native workspace command continues through all targets after failures. It records 1,455 passes, one source-transfer size failure and eleven ignored entries across 66 result groups, including two empty doctest groups. Formatting and strict workspace Clippy pass. The complete logical source payload is 33,866,451 bytes, exceeding the unchanged 32 MiB cap by 312,019 bytes; no successful transfer/restore is claimed. The proposed source-cap change remains unapproved. All fixed corpora and vendor sources are retained. These host results do not qualify the current source on GitHub CI, other platforms, final shipping archives, performance or cutover. JavaScript 0.5.2 remains the shipping default.

## Same-instance evaluator updates and cancellation admission

The [current checkpoint](../rust/parity/measurements/native-router-evaluator-reconfiguration-summary.json) adds `Router::reconfigure_evaluator(&mut self, &RouterConfig)` for library-owned routers between route borrows. It copies evaluator settings and exact source values while retaining active tasks, staged attempts, confirmed opaque tool ownership, classifier and token caches, worker owners and closed state. It does not reload an `Arc`-owned gateway. All eight new regressions pass in the complete 1,463-pass native run. The sole failure remains the unchanged development source-transfer cap: logical payload is 33,891,268 bytes, 336,836 bytes over its limit.

The [additive per-assertion API review](../rust/parity/assertion-mappings/router-concurrency-cancellation-settings-api-migrations.json) accepts only original router-concurrency definitions 3, 4, 5, 7 and 10: 26 assertion sites and 27 original observations. Typed cancellation, first-polled futures and owned worker/body lifetimes replace only the identified JavaScript error, signal, Promise and configuration-alias interfaces. Actual admitted-before-I/O cancellation and a completed old success held at publication cover the previously missing native triggers. Empty pending registry does not imply retiring worker capacity has been released. Historical mappings, captures and accepted definition 9 remain unchanged. Coverage is 550 covered and 38 partial definitions; keys, limits and all thirteen area statuses remain unchanged.

Ordinary JavaScript checks also pass: 97 syntax/contract files, all 596 tests and installed package lifecycle. The first Clippy style failure and JavaScript fixture-mode failure under the private evidence owner’s umask remain retained beside corrected passing runs; no assertions or limits were weakened. Earlier workspace/CI reports remain historical. JavaScript 0.5.2 is still the shipping default; current CI, platform, performance, release and cutover gates remain open.

## Buffered response cancellation across sibling requests

The [current checkpoint](../rust/parity/measurements/native-buffered-sibling-isolation-summary.json) adds actual HTTP, TLS 1.2 and TLS 1.3 schedules through the buffered native raw pool. A downstream TCP reset cancels A while the same-origin raw B and library Fetch GET C, and other-origin raw D, remain held. B/C/D then deliver their exact complete entities; B/D followups reuse their respective physical provider connections. All three native tests and their frozen JavaScript comparisons pass. Per-request registered identities receive one delivery claim and one finished callback; the executed native tests also pass their subsequent normal cleanup assertions. The JSON markers are emitted before cleanup, rather than representing a post-cleanup resource census.

Two test-oracle errors were corrected against actual wire behavior and ownership code: both gateways emit chunked responses, and claimed deliveries finish through `CallbackFinished` rather than the alternative unclaimed-drop `Retired` event. All four failed native runs remain retained. The corrected tests enforce bounded framing, exact body bytes, connection identities and callback cardinalities without changing production code, deadlines or existing Source-compatible TLS ticket eviction. These controls do not qualify TLS resumption, malformed parsers, arbitrary cancellation/crash cleanup or the complete transport matrix.

The full ordinary current workspace records 1,466 passes, one unchanged source-transfer size failure and eleven ignored tests, with no filtered tests. The logical source payload is 33,919,872 bytes, 365,440 bytes over the unchanged 32 MiB cap; no successful transfer or cap change is claimed. Earlier JavaScript checks remain applicable to all 130 unchanged shipping files and are not represented as new executions. The ledger remains 550 covered and 38 partial definitions, twenty accepted keys, twenty-seven accepted limits and all thirteen areas partial. Current CI, supported-platform, numerical performance, release and default-cutover gates remain open.

## Individual Ollama model and keep-alive configuration acceptance

The [current checkpoint](../rust/parity/measurements/native-ollama-configuration-keys-summary.json) accepts only `AUTOROUTER_OLLAMA_MODEL` and `AUTOROUTER_OLLAMA_KEEP_ALIVE` as individual keys. All 71 complete parser outcomes match frozen JavaScript: eighteen full configurations and fifty-three exact errors, including fifteen line-ending rejections. The same integration test executes seventeen private configuration commands against each engine, covering default/file/environment provenance, empty overrides, inactive settings, set/unset and read-only reports. Existing raw JSON, exact-value ownership and named request/cache/warmup consumers are composed explicitly; no real evaluator or provider is qualified.

Three new core tests and one CLI test pass in the full current workspace. The separate five-depth experiment records equal complete output at 63, 64, 80 and 2,048 array layers. At 20,000 layers, JavaScript parses the input but `readConfig` raises `RangeError` before serialization; Rust admits and retains the exact owned value after dropping the source document. The [embedding guide](../rust/docs/embedding.md) explicitly adopts this JSON-backed resource/API difference, preserving ordinary grammar, error order and input bounds without inventing a portable V8 threshold. It is an accepted-input extension, not a claim of Source 20,000-layer success.

Coverage remains 550 covered and 38 partial original definitions, with twenty-two accepted keys and five pending keys, twenty-seven accepted limits and all thirteen areas partial. The complete ordinary run records 1,470 passes, one unchanged source-transfer size failure and eleven ignored tests, with no filtered tests; formatting and strict workspace/all-target Clippy pass. Logical source payload is 33,935,381 bytes, 380,949 bytes over the unchanged 32 MiB cap. All 130 shipping files remain unchanged; their earlier JavaScript checks are retained historical executions. Current CI, supported-platform, performance, release and default-cutover gates remain open.

## Session-log directory and three endpoint configuration keys

The [session-directory checkpoint](../rust/parity/measurements/native-session-log-directory-configuration-key-summary.json) accepts `AUTOROUTER_SESSION_LOG_DIR` individually. Two regressions execute fourteen private CLI commands against each engine. They cover saved/default/environment provenance, empty-disable behavior, setter validation and exact UTF-16 reports. An escaped unpaired surrogate and a literal replacement character retain distinct saved values while reading the same private UTF-8 history file; complete history reports and unchanged file/directory identities are checked. Logging remains disabled by default.

The [endpoint checkpoint](../rust/parity/measurements/native-url-configuration-keys-summary.json) individually accepts `AUTOROUTER_UPSTREAM_URL`, `AUTOROUTER_JEV_URL` and `AUTOROUTER_OLLAMA_URL`. Each engine passes twenty-four configuration commands covering literal storage, normalized effective values, runtime overrides, forced setter validation, inactive masking and full checks. Existing constructor, subscription, policy and downstream consumer evidence is composed explicitly. No remote endpoint or real local evaluator is contacted.

Fifteen parsed URL-depth controls record twelve matching complete configurations at 63, 64, 80 and 2,048 array layers. At 20,000 layers Source parses each input but returns its safe URL validation error before config serialization; the inner exception is not exposed. Rust accepts and detaches the normalized endpoint. The [embedding guide](../rust/docs/embedding.md) explicitly adopts this separate JSON accepted-input resource extension; ordinary OS settings, safety/account guards and input bounds are unchanged.

The current unfiltered workspace records 1,474 passes, one unchanged source-transfer size failure and eleven ignored tests across sixty-four executables and two empty doctest groups. All prior test statuses remain, with four new passing regressions. Formatting and strict workspace/all-target Clippy pass. Coverage is 550 covered and 38 partial original definitions, twenty-six accepted keys with only `AUTOROUTER_TOKEN` pending, twenty-seven accepted limits and all thirteen areas partial. Logical source payload is 33,955,509 bytes, 401,077 over its unchanged 32 MiB cap; the expanded carrier is 375,808 bytes over its separate unchanged cap. The larger-cap proposal remains unanswered and unapplied. Historical checks for all 130 unchanged JavaScript shipping files remain historical; JavaScript 0.5.2 still ships. Current CI, supported-platform, performance, release and default-cutover gates remain open.

## Final configuration key and explicit library API migrations

The [local-token checkpoint](../rust/parity/measurements/native-local-token-configuration-key-summary.json) accepts `AUTOROUTER_TOKEN` individually, completing all twenty-seven key rows. Eight exact raw HTTP packets per engine establish genuine Authorization-only fallback, duplicate Authorization first-value behavior, custom-header precedence including an empty field, Latin-1 credential bytes and comma-space duplicate joining. All statuses, complete response entity bytes, packet identities and full configuration hashes match. Owned socket/listener cleanup and zero backend calls are observed within the declared finite controls. These checks need no authentication implementation change and do not qualify the entire gateway protocol matrix. The existing ten raw nonstring constructor differences remain explicit native typed-string API migrations.

A separate [per-assertion checkpoint](../rust/parity/measurements/native-config-model-request-assertion-migrations-summary.json) accepts only config definition #6 and model-request definitions #4, #5 and #7. Config #6 has twenty-two finite comparisons and an actual Number Infinity rejection; its literal NaN assertion executed in JavaScript, while native NaN remains outside the supported JSON input API and is not reported as a native guard execution. For request preparation, the two own `thinking: undefined` fields become absent optional JSON fields, and eighteen JavaScript shallow-reference observations become complete detached owned values. All nineteen original schedules, complete signed history, safeguards, opaque fields and input-preservation checks are bound to actual Source and native passes. The [embedding guide](../rust/docs/embedding.md) states the absence and ownership changes explicitly. Two unavailable historical model-mapping evidence paths are disclosed and are not credited; current retained execution and ownership evidence supplies the reviewed replacement. Original mappings and skipped-input history remain unchanged.

Coverage is now 554 covered and 34 partial original definitions, twenty-seven accepted keys, twenty-seven accepted limits with twenty-six pending, and all thirteen areas partial. No runtime code or checked-in test changed in this checkpoint. The previously executed 1,474-pass workspace, formatting and strict Clippy checks retain the same 697 charged source inputs; the later documentation-only auxiliary drift is recorded separately. No new broad run or all-green suite is claimed. Both source-transfer caps remain unchanged and exceeded by 401,077 logical bytes and 375,808 carrier bytes. The larger-cap proposal is still unanswered and unapplied. Current CI, remaining behavior/resource/platform/performance gates, release and default cutover remain required; JavaScript 0.5.2 continues to ship.

## Evaluator state budgets and consumed content depth

The [state-budget checkpoint](../rust/parity/measurements/native-evaluator-state-default-limits-summary.json) individually accepts the two fixed evaluator defaults. Each engine makes four classifier calls over one unchanged synthetic request: one classification and one cache hit per backend. Ollama produces an identical 2,969-byte state using 1,747 UTF-16 units under its 3,000-byte limit. Jev produces an identical 11,999-unit state using 20,733 UTF-8 bytes under its 12,000-UTF-16-unit limit; Jev has no corresponding 12,000-byte ceiling. All complete request bodies, headers, states, configurations and decisions match. The three initial transport requests per engine are injected mocks, and cache hits add none. Successful response EOF/release, public native worker drain and transport ownership checks pass. No real model, provider call or performance measurement is involved.

The [content-depth checkpoint](../rust/parity/measurements/native-consumed-content-nesting-limit-summary.json) individually accepts the fixed limit of 32 recognized tool-result array-content edges, starting top-level content arrays at zero. Retained paired 31/32/33 controls and current 32/33/100 expected-value tests establish the boundary; strings end traversal and opaque provider fields remain unwalked. The deep opaque 4,096-layer comparison is explicitly historical, and pre-backend enforcement is a static composition of both endpoints. Neither is reported as a fresh current deep-wire test or complete gateway qualification.

Coverage remains 554 covered and 34 partial original definitions, with all twenty-seven configuration keys accepted and all thirteen behavior areas partial. Thirty fixed limits are accepted and twenty-three remain pending. No runtime code, checked-in test, dependency or fixed corpus changed. The earlier 1,474-pass workspace, formatting and strict Clippy checks remain same-code evidence; the one source-transfer failure and eleven ignored tests are retained. Logical and expanded source payloads still exceed their unchanged caps by 401,077 and 375,808 bytes respectively, with the larger-cap proposal unanswered and unapplied. Current CI, protocol/resource/platform/performance qualification, release and default cutover remain required; JavaScript 0.5.2 still ships.

## Session retention, exact queue capacity and telemetry API boundaries

The [retention checkpoint](../rust/parity/measurements/native-session-retention-default-limits-summary.json) accepts the fixed 128-session log registry and 100-session status retention limits individually. These constrain separate storage structures; neither establishes a process-wide descriptor or active-task limit.

The [queue checkpoint](../rust/parity/measurements/native-session-log-pending-byte-default-limit-summary.json) adds paired public-writer controls at exactly 1,048,576 normalized JSONL bytes and one byte over. Across both engines, all 1,244 admission decisions and all 1,234 durable rows match; the full log bytes are identical. Accepted records drain, overflow remains sticky and each case warns once. Two earlier Source attempts failed during harness setup because filesystem guards blocked Node's loader; both remain retained. The reviewed correction serves only preverified reference bytes during import and restores denial afterward. Product code, fixed corpora and production capacities remain unchanged.

The [status and telemetry checkpoint](../rust/parity/measurements/native-status-telemetry-original-summary.json) accepts eight original definitions individually after reviewing 52 assertion sites and 74 expanded original observations. Three new native regressions verify the full telemetry allowlist and privacy, required-field absence with valid positive controls, and malformed empty input without mutation. The [embedding guide](../rust/docs/embedding.md#status-and-telemetry-host-boundaries) states the exact typed JSON and owned-close-future boundaries. Source Error instances, getters, cycles, own undefined and Promise identity are not falsely reported as native executions. Ten unavailable historical artifact paths remain disclosed; retained original execution and current native tests provide the reviewed evidence.

Coverage is 562 covered and 26 partial original definitions, all twenty-seven keys accepted, thirty-three accepted fixed limits with twenty pending, and all thirteen areas partial. The current unfiltered workspace records 1,477 passes, one unchanged source-transfer failure and eleven ignored tests across sixty-four executables and two empty doctest groups. Every previous test status remains, with exactly three new passes. Formatting and strict Clippy pass; the existing vendored hyper-util warning is retained. No fresh JavaScript suite or current CI is claimed. Logical source size is 33,960,442 bytes, 406,010 above its unchanged 32 MiB cap; expanded carrier size is 380,928 bytes over its separate cap. This is arithmetic evidence, not a successful transfer. The larger-cap proposal remains unanswered and unapplied. Protocol/resource/platform/performance, release and cutover gates remain open; JavaScript 0.5.2 still ships.

## Diagnostic, setup, log API boundaries and four default limits

Eight further original definitions have individual acceptance: local diagnostics 6, 12 and 13; Ollama setup 9, 10 and 14; and session logging 3 and 6. The [per-assertion map](../rust/parity/assertion-mappings/local-setup-session-log-native-api-migrations.json) preserves all 42 original assertion sites and 81 expanded observations. Typed configuration, payload-free cancellation tokens and inert finite JSON metadata replace specific JavaScript host operations. NaN reports, arbitrary Error identities, getter evaluation, Infinity admission, AbortSignal properties and cancellation Promises are not relabeled native executions. Actual polled body-release and durable-file controls remain separate. Missing historical raw artifacts remain disclosed.

The [three default-limit review](../rust/parity/measurements/native-prompt-status-timer-default-limits-summary.json) accepts the production 500-code-point excerpt and 1,000 ms readiness and 5,000 ms heartbeat defaults. The two status timers use mock or paused clocks; no unexecuted 999/4,999 ms checkpoint or wall-clock performance claim is inferred. The [Jev deadline comparison](../rust/parity/measurements/native-jev-default-deadline-summary.json) separately exercises the unchanged 1,500 ms default using real clocks and injected transports. Both response-head and response-body stalls produce the complete fallback, release owned work and permit a successful retry followed by a cache hit. All complete common configurations, decisions and request bytes match; Source and native cleanup witnesses retain their distinct interfaces. No provider call was made.

Coverage is now 570 covered and 18 partial original definitions, all 27 configuration keys, 37 accepted fixed limits and 16 pending limits. All thirteen behavior areas remain partial. No production code, fixed corpus, vendor input, source cap or JavaScript shipping file changed in this increment. The current 1,477-pass workspace evidence still has the known source-transfer cap failure and eleven ignored tests; it is not a fresh execution after these auxiliary documentation and evidence updates. HTTP timeout scheduling, full resource/platform/performance qualification, release and cutover remain open. JavaScript 0.5.2 still ships.

## Scanned HTTP receive timers and current workspace checkpoint

The [HTTP timer checkpoint](../rust/parity/measurements/native-http-scanned-receive-timers-summary.json) repairs three actual Gateway differences against frozen JavaScript on the same unchanged three-socket diagnostic. Blank connections now return the exact 51-byte raw 408 near the first 30-second scan. Partial headers completed at 20 seconds return the complete 200 health entity, and a body completed at 35 seconds returns the complete 400 invalid-model entity. Earlier native runs instead left the blank connection open, closed the partial header near 11 seconds and rejected the body near 31 seconds. Complete response entities match; Date and chunked-versus-Content-Length framing differ. This is not whole-wire equality or a provider benchmark.

The Gateway now owns a shared listener scanner with atomic parser receipt events and per-response preparation markers. Delivery failures are claimed before publishing a timeout, and an eligible raw 408 uses one best-effort write without a blocking flush or shutdown. The existing first-send body gate retains cancellation/deadline checks before replaying provider bytes. Twenty new native regressions cover strict thresholds, queued/current responses, parser ordering and terminal writer behavior. Six are Runtime counterparts using the actual public Hyper API; the separate vendor unit executable was not run. The full vendored patch/provenance and independent auditor pins were refreshed; offline reconstruction and isolated native source audits pass with upstream/archive/legal inputs unchanged.

Only the numeric header and total-message defaults are newly accepted: 10,000 and 30,000 ms are strict receipt ages checked at 30,000 ms listener scans, rather than separate wall deadlines. Fixed-limit coverage is 39 accepted and 14 pending. Original definitions remain 570 covered and 18 partial, all 27 configuration keys remain accepted, and all thirteen behavior areas remain partial. The delayed second actual listener callback, generic parser-error blocked-output cleanup, full HTTP/TLS/platform/resource/performance and release gates remain separate.

The current unfiltered workspace records 1,497 passes, the one unchanged source-transfer size failure and eleven ignored entries across 64 executables and two empty doctest groups. Every prior status remains, with exactly twenty new passes. The ordinary CLI build, formatting and strict workspace/all-target Clippy pass. The initial denied-loopback run, auditor environment mistake and three corrected Clippy style errors remain retained. Current logical source size is 34,106,310 bytes, 551,878 over its unchanged 32 MiB cap; expanded carrier size is 527,360 bytes over its separate cap. The larger-cap proposal remains unanswered and unapplied. Historical checks for unchanged JavaScript shipping inputs remain historical; JavaScript 0.5.2 still ships. No commit, push, CI run, release, publication or cutover occurred.

## Bounded cleanup and strict release checksum inspection

The [current checkpoint](../rust/parity/measurements/native-cleanup-checksum-deadline-summary.json) replaces unbounded cleanup-name retention with a bounded max heap while preserving the same first 200 prefix candidates before trust checks. Unsafe candidates do not refill the selection. Nineteen actual filesystem cases match frozen JavaScript in every common report field: candidate counts, a skipped regular-file slot, valid snapshots one byte below/at/above 1 MiB, and grace/heartbeat ages one millisecond below/at/above their defaults. Complete surviving fixture bytes and identities are preserved. The 200-candidate limit is not a total traversal, syscall, CPU or RSS bound; age tests use injected current time.

The release inspector now rejects trailing checksum whitespace and checks exact filename identity before digest equality. The command emits the original fixed safe SHA256, exact-filename and regular-file diagnostics; unknown errors stay generic. Two actual pre-fix regressions are retained beside the corrected passing runs. Final native authorization already required canonical sidecars and remains unchanged. The [three-assertion supplement](../rust/parity/assertion-mappings/release-checksum-native-api-migrations.json) accepts only original release-check definitions 8, 9 and 16, using the explicit native fixture and typed-error/command migration. Parent definition 7 remains partial. Synthetic approvals do not qualify a real release.

Original coverage is now 573 covered and 15 partial, with all 27 configuration keys and 43 accepted fixed limits; ten limits and all thirteen areas remain incomplete. The separate new token-count regression proves independent default 1,500 ms budgets under a paused clock, cancellation, retries and caching. Its numeric row stays pending because no matching Source default-timeout execution is claimed.

The current unfiltered workspace records 1,503 passes, one unchanged source-transfer size failure and eleven ignored entries across 64 executables and two empty doctest groups. All previous results remain, with six new passing tests. The 581-test Runtime run, 53 focused release tests, 16 unchanged Source release tests, formatting, strict Clippy and ordinary CLI build pass. Charged source size is 34,135,344 logical bytes, 580,912 over the unchanged 32 MiB cap; the expanded carrier exceeds its separate cap by 556,032 bytes. A fresh structural-sharing assessment found no credible small refactor that eliminates the deficit while retaining the fixed corpus. The larger-cap proposal remains unanswered and unapplied. Current CI, remaining protocol/resource/platform/performance, final distribution/provenance, stabilization and cutover remain open. JavaScript 0.5.2 still ships; no commit, push, CI, release or publication occurred.

## Release file policy and complete public materials

The [release file-policy checkpoint](../rust/parity/measurements/native-release-file-policy-summary.json) retains safe, specific diagnostics for an unexpected private package member and an extra release-directory archive. Both original synthetic mutations were reproduced before the fix, including complete error output and preserved source/artifact bytes. A lone authorization checksum now reports the incomplete artifact set; a separate both-files-absent control preserves the final-authorization requirement. Unknown causes and arbitrary detail stay generic.

The native package and direct-archive validators now require all public materials selected by the producer. Before the fix, both validators accepted a self-consistent archive omitting `CONTRIBUTING.md`. New regressions check all 22 native and 21 direct omissions, canonical baselines and the separate original `LICENSE` deletion with stale declarations. This proves admission completeness; it does not establish a final-authorization bypass or full Source package-list equivalence.

Only original release-check definitions 10 and 15 receive [individual native-format/API acceptance](../rust/parity/assertion-mappings/release-file-policy-native-api-migrations.json). Original coverage is 575 covered and 13 partial; parent 7 and definitions 11–14 remain partial. The 27 configuration keys and 43 accepted numeric defaults are unchanged. All thirteen areas remain incomplete.

The current unfiltered workspace records 1,507 passes, one known source-transfer size failure and eleven ignored entries across 66 result groups, retaining every previous result and adding four passing regressions. All 57 focused release tests, formatting, strict Clippy and the ordinary CLI build pass. The fixed 32 MiB source caps remain unchanged: logical payload is 34,152,715 bytes (598,283 over), and the carrier is 573,440 bytes over its separate cap. The larger-cap proposal remains unanswered and unapplied. Platform, performance, remaining resource/protocol, full distribution/provenance, protected CI, stabilization and cutover remain open. JavaScript 0.5.2 still ships; no commit, push, CI or release occurred in this checkpoint.

## Token-count and pull default limits

The [individual limit comparison](../rust/parity/measurements/native-count-and-pull-default-limits-summary.json) closes only the 1,500 ms token-count default, the 16 MiB cumulative pull-response limit and the Source-defined 65,536-byte nonblank-line/pending-buffer checks. Two real-clock frozen JavaScript timeout schedules and four actual response streams pass; the unchanged native deadline and stream-boundary tests already passed on source `7e30938`. Source late fixture release and native Drop remain separate cleanup evidence. Completed whitespace-only lines retain their length-check exception, and LF is excluded from raw-line length. No Rust source or compiler run was needed for these comparisons. Fixed limits are now 46 covered/7 pending; original definitions remain 575 covered/13 partial and all 27 configuration keys remain covered. The existing 1,507-pass/one-source-cap-failure/11-ignored workspace result is retained. The 32 MiB source-transfer caps, whole-area approval, platform, performance, release and cutover gates remain unchanged.

## Inspection deadline and layered archive integrity

The [inspection comparison](../rust/parity/measurements/native-inspection-default-deadline-summary.json) accepts only the default 5,000 ms budget shared by version, tags and show. Four frozen JavaScript schedules hold early or late response heads and bodies; all 88 declared predicates, 20 complete requests and 18 response bodies match the declared outcomes. The native regression checks ownership at 4,999 ms and timeout at 5,000 ms, including late cases that have already spent four seconds on earlier endpoints. Each schedule then succeeds with the same active caller. JavaScript abort cooperation and settling stream cancellation remain distinct from native future/body Drop; these tests measure no model performance.

Three additional [layered archive controls](../rust/parity/measurements/native-layered-release-integrity-summary.json) test a modified executable with recomputed internal file declarations, a known target outside the selected release matrix, and a coherently qualified archived manifest against an unchanged checkout. The immutable candidate index, safe errors, complete positive metadata and named material checks remain enforced. Five original release definitions, including the parent and its nine individually reviewed children, receive explicit native-format/API migration acceptance. This does not claim literal JavaScript archive-format or diagnostic equality where the native interface differs. Coverage is now 580 covered/eight partial originals and 47 covered/six pending limits; all 27 configuration keys remain covered and all thirteen areas remain incomplete.

Current source `99f2bab` retains all 1,519 previous workspace outcomes and adds four passes: 1,511 passes, one known source-transfer size failure and eleven ignored tests across 66 result groups. All 60 focused release checks, formatting, strict Clippy and the ordinary CLI build pass; an existing vendored warning remains. Node checks, all 596 tests and the offline 58-file package check pass. The initial ten Node failures are retained: scratch files under the ESM repository broke nine CommonJS fixtures, and the runner's restrictive umask changed one mode assertion. Moving owned scratch space outside the repository and restoring the ordinary child umask fixed the unchanged tests.

The fixed 32 MiB transfer caps remain unchanged. Logical payload is 34,170,924 bytes, 616,492 over; the expanded carrier is 34,146,304 bytes, 591,872 over its separate cap. These are independently reviewed size calculations, not a successful export/restore. The larger-cap proposal remains unanswered and unapplied. The latest cleanup reclaimed 424,263,680 allocated bytes through lossless filesystem compression, preserving source and evidence; the package runner's owned external scratch directory was also removed. Current protected CI, remaining tooling/protocol/resource/platform/performance qualification, final provenance, stabilization and cutover remain open. JavaScript 0.5.2 still ships; this checkpoint performs no commit, push, workflow dispatch or release.


## Tooling policy and preflight checkpoint, 2026-10-18

The [reviewed summary](../rust/parity/measurements/native-tooling-policy-preflight-summary.json) individually accepts four original definitions: package-lifecycle 2, release-verification 2 and 13, and evaluation-report 12. Six named API migrations retain private nonzero diagnostics, closed safe network errors, native eligibility for new publication, typed nonfinite rejection, owned policy admission and evaluator-call ordering. The unchanged valid evaluation control returns a complete outage report after three synthetic transport errors; it does not establish evaluator success. Original JavaScript freeze and factory identity operations remain documented differences.

Only three existing xtask test modules changed from source `99f2bab`; production, dependencies, fixed corpora, vendor material and caps remain unchanged. Current source `895e5f5` passes 1,514 Rust tests with one source-transfer failure and eleven ignored tests across 66 groups. All 1,523 preceding statuses remain, plus three passes. Formatting, strict Clippy and the default CLI build pass; the existing vendored hyper-util warning remains. All 596 shipping Node tests and package checks (58 safe files, 17 mock lifecycle scenarios) pass with fresh isolated Node 22 execution. The first strict Clippy failure and narrow assertion correction remain retained. These local checks do not qualify current changes through protected remote CI or an immutable native release archive.

The ledger now contains 584 covered and four partial original definitions, with 47 covered and six pending fixed limits, all 27 configuration keys covered and all thirteen behavior areas still partial. Remaining originals cover source-bundle execution without a Rust toolchain, lifecycle helper readiness/signals, structured missing-executable outcomes and the complete installed release-verification command chain.

Source-transfer census 40 records 34,186,049 logical bytes (631,617 above its limit) and 34,161,664 expanded carrier bytes (607,232 above its limit). Both ceilings remain 33,554,432 bytes. This is arithmetic corroboration of the failing native test, not a successful export/restore or compressed archive measurement. Earlier cleanup reclaimed 424,263,680 allocated bytes through same-path APFS compression with zero file deletions; completed owned Node temporary directories were subsequently removed separately. Work and earlier evidence remain preserved. JavaScript 0.5.2 remains the shipping default, and the full rewrite goal remains active.


## Subprocess spawn-cause checkpoint, 2026-10-18

The [reviewed subprocess change](../rust/parity/measurements/native-process-spawn-cause-summary.json) repairs a real information loss: the native tool runner previously reported missing files and denied execution with the same generic flag. It now retains only `not_found`, `permission_denied` or `other`, without OS error text, command paths or arguments. The strict live-report consumer discards this internal field and preserves its existing public shape. Real owned missing/0600/NUL fixtures, the original configured 100 ms stalled-child case, an exit-17 control and complete three-label consumer results cover the change. Tests first produced the two intended assertion failures, then passed unchanged after the narrow repair.

Only `tool_process.rs` and `live_claude.rs` changed from source `895e5f5`; all other 699 source inputs, frozen files, dependencies, corpus, vendor material and caps remain unchanged. Current source `9bca0e4` records 1,517 Rust passes, the same source-transfer failure and eleven ignored tests in 66 groups, preserving all 1,526 prior outcomes and adding three passes. Formatting, strict Clippy, the default CLI build and all shipping Node/package checks pass. The first sandbox-limited workspace run (1,250 passes, 268 failures) remains retained; the retry changed tool permissions only. Both focused scopes still report unconfirmed process-group cleanup with raw errno 1, so this checkpoint makes no whole-group cleanup guarantee or claim about its cause. The default CLI hash was captured immediately after the final ordinary build; an earlier late observation of a workspace feature-union executable remains recorded separately.

Explicit API decisions replace the original Promise rejection and JavaScript error identity with the direct native runner's deadline metadata and closed cause. Only package-lifecycle definition 3 becomes covered: the ledger is 585 covered/three partial originals, with six fixed limits and all broader gates still open. Census 41 is 34,193,357 logical bytes (638,925 over) and 34,168,832 carrier bytes (614,400 over); both ceilings remain 33,554,432. This is arithmetic corroboration, not a successful source export. The newly owned Node temporary directory was removed after all three checks passed; earlier work and evidence remain preserved. JavaScript 0.5.2 remains the shipping default.

## Readiness and startup-budget checkpoint (2026-10-18)

The [scoped checkpoint](../rust/parity/measurements/native-readiness-release-main-checkpoint-summary.json) records the owned readiness helper, its six original signal observations, eight paired default pull/warmup deadline schedules, and automatic Cargo tests of the actual release-verification main. The helper accepts INT/TERM from its caller and never derives a process identity from readiness JSON. Native chunk boundaries and structured failure/grace behavior are explicitly reviewed API differences; raw cleanup failures remain visible. The synthetic release fixture is header-only and cannot establish the missing runnable-archive install chain.

Current unfiltered results are 1,525/1/11 across 66 groups: all 1,529 previous statuses plus eight new passes. The sole failure remains the unchanged source-transfer cap. Census 42 computes 34,252,986 logical bytes (698,554 over) and 34,228,224 expanded carrier bytes (673,792 over); this is arithmetic, not a successful export/restore. No cap, frozen corpus or threshold was changed. Cleanup preserved source, evidence and runnable builds through lossless compression; final post-test compression reclaimed 187,584,512 allocated bytes, without claiming a net volume gain. JavaScript 0.5.2 remains the shipping default and the full goal remains incomplete.

The [runnable release checkpoint](../rust/parity/measurements/native-runnable-release-chain-summary.json) closes the missing same-archive chain: a harmless Cargo-built native fixture passes production inspection, real offline npm installation and installed version/help, followed by submitted and corrupted-checksum reports through the actual built main. The shared physical command entrypoint preserves existing unit-test selectors; the standalone auditor still excludes runtime/vendor dependencies. Native archive format, typed operation boundaries and entrypoint/error projections are three explicitly accepted differences for original release-verification definition 14. Synthetic approvals and the fixture executable do not qualify a product release.

The [exact savings-history boundary](../rust/parity/measurements/native-savings-history-default-limit-summary.json) accepts only the default 10,000-entry numeric row. Fourteen complete Source/native snapshots cover below/at/above the threshold, FIFO duplicate retention, session scope and clear, with all totals and pricing provenance preserved. Coverage is now 587 covered/one partial original and 50 covered/three pending fixed limits; all 27 configuration keys remain covered and all thirteen behavior areas remain incomplete.

Current source12ba1490 has 705 inputs. The unfiltered workspace records 1,527 passes, one unchanged Source-transfer size failure and eleven ignored entries across 66 groups; all 1,537 previous statuses remain, plus two passes. Formatting, strict Clippy, ordinary default CLI build and all shipping Node checks pass: 596 tests and the 58-file package with 17 mock lifecycle scenarios. The default CLI retains its prior byte identity. Census43 is 34,289,247 logical bytes (734,815 over) and 34,265,088 carrier bytes (710,656 over); both caps remain 33,554,432. This is static arithmetic, not an admitted source export or compression measurement. The final cache cleanup reduced allocation by 1,068,023,808 bytes through same-path compression without deleting files, preserving all source and work backups. JavaScript0.5.2 remains the shipping default; performance, platform, live-quality, immutable-release and cutover gates remain open.

The [final default-limit review](../rust/parity/measurements/native-final-default-limits-summary.json) individually accepts the ten-minute upstream deadline and both archive byte-limit rows. The Source helper’s uncapped compressed decoder and the reviewed native 64 MiB shipping expansion remain explicit API/policy differences; historical and source expansion stays 32 MiB. All previous workspace statuses are retained with three additional passes. The default source/reference transfer still fails its unchanged combined 32 MiB budget. The separate 40 MiB schema7 proposal passed isolated export/restore and 30 prospective-default bundle tests, but requires an explicit decision before adoption. The ready executable and source bundle are still separate artifacts, so original benchmark-bundle definition 1 remains partial.

## Host and GNU CI failure preservation

The [local workflow validation](../rust/parity/measurements/native-ci-independent-stages-summary.json) retains complete workspace logs and original failure statuses, adds Cargo `--no-fail-fast`, and gives later stages explicit prerequisite conditions. Release checks require successful bootstrap and both auditors; source comparisons require a separate successful reference freeze; installed checks require the exact package stage. None of these paths converts a failing required job into success. All 24 extracted-shell failure controls, 59 Bash syntax blocks and six workflow AST checks pass. This is local synthetic evidence; fresh remote CI and the separate musl phase split remain pending.


## Same-artifact benchmark sources and CI checkpoint (2026-10-18)

The [current scoped summary](../rust/parity/measurements/native-same-artifact-and-ci-summary.json) records one retained 10,703,652-byte archive containing the runnable evaluator helper and all 706 native source inputs. After relocation, help runs with an empty PATH, and extraction reproduces all 708 materialized source files with exact bytes, hashes and modes. The complete composed charge is 55,486,976 bytes within the unchanged 64 MiB native bound; the inner source retains its separate 32 MiB ceiling. The distinct native format, three exact public vendor privacy exceptions, and native help entrypoint are individually reviewed API migrations. The inner source manifest remains schema2; outer schema1 is a different kind. Local build provenance remains `unverified_prebuilt` for release purposes.

This closes only the last original benchmark-bundle definition: 588/588 original definitions, 53/53 fixed limits and 27/27 configuration keys now have individual acceptance. All thirteen broader behavior areas and the full rewrite remain incomplete. Current source `18107a0e` preserves all 1,542 previous workspace statuses and adds one pass: 1,531 passed, one unchanged joint-source-transfer failure and eleven ignored tests in 66 groups. The three required bundle integrations, formatting, strict Clippy, ordinary CLI build, shipping Node checks, all 596 Node tests and the 58-file package check with 17 mock lifecycle scenarios pass. The initial Node mode assertion failure is retained; a child-only ordinary umask fixed that unchanged fixture.

Host, GNU and musl workflows now preserve full workspace failures and run later independent stages only after their own successful prerequisites. All 67 local failure controls pass, including 43 musl phase/controller and malformed-gate cases. No remote CI or real musl/Docker qualification was performed. The unchanged joint 32 MiB source-transfer failure stays red; schema7/joint40 remains an isolated, unapproved proposal. Failed-startup rollback, broader protocol/resource/platform/TLS, representative hardware/performance, live quality, final release provenance, stabilization and cutover remain pending. JavaScript 0.5.2 remains the shipping default.

## Installed startup failure recovery

The [current local checkpoint](../rust/parity/measurements/native-installed-failed-startup-recovery-summary.json) adds two same-prefix installed startup failures before rollback: a controlled Claude child exits 23 after copying its live status/settings, and an empty PATH triggers the missing-Claude error. All three fixed final-source trials pass with the same immutable private native archive. They verify private live resources and cleanup, a closed gateway, unchanged synthetic configuration/history bytes and metadata, and successful rollback/re-upgrade. The shared bounded report validator checks the new evidence in host, GNU, musl and runtime32 CI consumers.

The first generic I/O failure and a later WouldBlock failure remain recorded. A separate 256-connection diagnostic reproduced two immediate positive-accept WouldBlock results; the control now waits only for that readiness condition, within 250 ms. The historical failures are not reclassified. All 67 local CI stage controls, 22 parser controls and 10 runtime32 adapter controls pass against unchanged CI code; their earlier source binding stays explicit. All three final-source operational reports separately pass the actual validator.

Formatting, strict workspace Clippy, the ordinary release build, three required source/executable bundle tests, all 596 Node tests and the 58-file/17-scenario JavaScript package check pass. The workspace retains all 1,543 prior outcomes: 1,531 passes, one unchanged joint 32 MiB source-transfer failure and eleven ignored tests. Fresh remote CI remains pending. This qualifies only the two synthetic host schedules; partial writes, concurrent ownership, direct rollback, real Keychain, other platforms and the broader plan remain open.


## Owned native npm smoke archive and installed lifecycle

The [current host checkpoint](../rust/parity/measurements/native-owned-npm-smoke-identity-summary.json) fixes a validation gap: npm previously reopened an archive path after verification, and the smoke path did not compare the full installed payload before execution. It now installs an exclusive private copy of the admitted bytes and checks the complete file tree, exact contents, full permission bits and resolved launcher target before running the CLI. Direct smoke also checks full permission bits. Replacement/removal and seven installed-mutation controls pass, while existing integrity vectors and assertions remain.

The exact retained npm archive and a direct archive derived from it each pass 92 installed CLI tests across seven suites on this Mac. Both contain the same ordinary executable, whose default build remains byte-identical and excludes development probes. An initial npm check failed safely before execution because the private owner's umask `077` masked all installed `644`/`755` modes to `600`/`700`. A metadata-only installation confirmed identical bytes and those modes. The retry changes only the child umask to ordinary `022`; private containers and staged archives remain `0700`/`0600`, and the permission predicate is unchanged.

Current source preserves every previous workspace status and adds one pass: 1,532 passed, one unchanged joint-source-transfer failure and eleven ignored entries. Formatting, strict Clippy, all three required source/executable bundle integrations, all 596 Node tests and the 58-file/17-scenario shipping package check pass. The existing vendored warning remains visible. These are local synthetic checks; fresh remote CI and all broader release, platform, performance and cutover gates remain pending. No source cap, frozen corpus, threshold, release version or shipping default changed.


## Closed musl SDK inputs

The [SDK follow-up](../rust/parity/measurements/native-musl-closed-sdk-input-summary.json) replaces mutable Alpine index resolution with the exact 61 package filenames and 122 architecture-specific SHA256 values already recorded by the prior builders. The base image, compiler, Rust, Node and tool versions are unchanged. Downloads are bounded and checked as they stream; Docker independently checks every digest before offline installation with APK signature verification enabled. Builder-input schema 3 records the closed package set rather than claiming a current index hash. The supplier-only 64 MiB per-package bound accommodates the pinned GCC package and changes no product or source-transfer cap.

All 49 downloader regressions pass without network calls, including both architecture manifests, corrupt or missing bytes, exact limits, deadlines and cleanup. The full Node suite passes 645 tests; syntax, contracts, style and the 58-file/17-scenario package check pass. Official HEAD observations cover availability and size for all 122 payloads; only three small payloads were actually downloaded and hashed locally. Full payload verification, signatures, SDK installation, report production and native rebuild require fresh remote execution. Raw native source remains `747307ee`; no new Rust workspace result or platform approval is claimed. The rewrite and its broader release gates remain incomplete.


## Runtime32 packaging child permissions and current CI

The [runtime32 follow-up](../rust/parity/measurements/native-runtime32-child-umask-summary.json) sets an ordinary `022` umask only for the npm/direct packaging children inside the existing i686 personality. The parent retains `077`, so diagnostic redirections remain `0600`. Exact installed modes, arguments, phases, exit statuses and private scratch permissions remain enforced. All seven extracted-workflow controls pass, and all ten shell blocks parse. The first local control attempt exposed older system Bash semantics for a false `[[ ]]` condition; its failure is retained, and the additive retry uses the installed, pinned modern Bash without changing any assertion or public workflow. These synthetic controls require fresh real i686 CI before installed lifecycle is credited.

At PR head `7ce3050`, retained terminal logs for both GNU targets, both Linux hosts and macOS ARM each show only the unchanged joint 32 MiB source-transfer failure; later independent checks succeed. Four Node jobs pass. Runtime32 stops before installed execution at the npm permissions check, and musl stops before Cargo at SDK input hashes. Intel remains unfinished in the retained snapshot. The merge checkout is recorded separately from the branch head. All required failures stay visible; the pending combined-budget proposal and broader rewrite qualification are unchanged.
