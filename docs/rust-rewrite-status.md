# Rust rewrite completion audit

This checklist tracks the full [rewrite plan](rust-rewrite-plan.md), including its cutover phase. It does not redefine completion as an experimental candidate, four convenient platforms, or a passing subset of tests. The frozen baseline is `ea930c247626ce2af5ccdad721b5121417bf4ad8` / JavaScript 0.5.2. Status reviewed on 2026-10-17; changes in progress require fresh validation before approval.

## Completion criteria and authoritative evidence

| Plan requirement | Evidence required | Current state / next work |
| --- | --- | --- |
| One native executable, every product command, no JavaScript sidecar | Installed command/JSON/exit-code matrix, Node-free execution and binary/build feature inspection | Implemented experimental candidate; host tests pass. Complete supported-platform execution remains open. |
| Keep package/command names, npm global installation and direct archives | Exact npm and per-target archive identities, scripts-disabled offline installation, installed byte/mode checks and lifecycle execution | Exact npm/direct host archives from clean `5ad9a76` each pass 78 installed lifecycle checks. Source-to-binary reproduction, other targets and the production release set remain open. |
| Retain all thirteen behavior areas | Individual original-assertion mappings, shared fixtures, Rust tests and explained interface differences | Coverage ledger: 538 covered, 50 partial and no pending definitions after the per-route counter and typed policy/configuration-error reviews. All 51 original Router definitions have individual mappings. Partial entries elsewhere still contain genuine missing behavior and explicit native API/format migrations; no whole area is blanket approved. Reviewed native API differences are recorded per assertion. Do not infer whole-area approval from fixture counts. |
| Transparent provider responses, signed history, safety and permissions | Actual HTTP/SSE/JSON/compression/framing comparisons, client disconnects, failed flushes and pipelined-response ownership | Substantial synthetic coverage; unresolved transport/TLS differences and unreviewed original HTTP schedules remain. |
| Authentication, refreshed subscription credentials and custom-upstream restrictions | Request-scoped credential tests, API/OAuth/beta rejection matrix, conservative unsupported-contract behavior | Implemented and tested on synthetic transports; live canaries and supported-platform TLS qualification remain. |
| Configuration, policy, Keychain and history without mandatory migration | Exact lexical identities, permissions/revision/fsync/no-follow checks, policy admission, isolated migration/rollback and existing schemas/prices | Isolated real macOS Keychain migration passes all 25 host assertions through both engines. Complete assertion review, failure/rollback and platform qualification remain. |
| Bounds, cancellation and state ownership | Full boundary/interleaving matrix; bounded retention, no uncancelled workers, clean shutdown, no false continuation confirmation | The [two classifier ceilings](../rust/parity/measurements/native-classifier-capacity-limits-summary.json) now have individual default, boundary, cancellation and cleanup review. The other 51 limit rows, all 27 configuration keys and the complete failure/resource matrix remain pending. |
| Predeclared speed and memory gates | Fresh matched five-round reports on representative hardware, unchanged thresholds, processing-only control, uncertainty and complete declared workload coverage | Exploratory v1 evidence exists. Five separately retained normal-use whole-processing five-pair runs retain complete parity; none qualifies the unchanged 0.5 p95 target. Numerical qualification, missing metrics/workloads and representative hardware remain open. |
| Real evaluator and task evidence | Explicit matched trials with fixed fixtures/rubrics/models and separately reported availability, transport, policy and task quality | Compatible and actual Auto simple Jev preflights pass. Two five-pair real Tev1 experiments retain identical warm classification failures, finite-deadline stress differences and matching zero-deadline outcomes. Full profile/task and larger-host evidence remain open. |
| Rust contributor/release tools | Original tool behavior mappings, release state handling, immutable archives, registry verification, direct installation and source/build provenance | Most tooling exists; original tooling assertions and production release workflow/provenance remain incomplete. |
| Native CI and supported platform matrix | Compiler/feature/license checks, installed native/npm/direct artifacts, minimum OS/libc/CPU/TLS/PTY/WSL evidence for every approved target | The [latest pushed `2d5defa` checkpoint](../rust/parity/measurements/native-2d5defa-ci-summary.json) finishes with thirteen successful jobs and one ARM musl SDK-setup failure before product compilation; its cause remains unproven. Intel completes in 32m17s and x64 musl passes. These metadata observations do not qualify later local changes or independently rehash artifacts. The earlier all-green `26412f2` result stays historical. Complete platform/minimum-OS/TLS/WSL and release qualification remain open. |
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

1. Finish the additive exact-archive upgrade/rollback CI checks on all current native packaging environments, retaining the successful and failed evidence separately.
2. Extend execution coverage to remaining baseline architectures with pinned toolchains and real target userlands; emulated execution must remain separate from CPU/minimum-kernel qualification.
3. Complete the remaining 50 original assertion definitions and configuration/limit audit, including explicit native API/format migration review rather than blanket suite approval.
4. Complete TLS/protocol/resource schedules and failed-startup, partial-write and ownership scenarios for rollback.
5. Meet every unchanged numerical gate and declared workload/hardware requirement, complete real-task evidence and final source/build/license/installed-artifact qualification, then perform separately authorized stabilization and cutover.

The latest fourteen green jobs at `26412f2` establish only their recorded scopes. Later local policy and capacity changes require fresh CI, and none of these slices replaces the full plan.

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
