# Development and validation

Use Node.js 22+ from a source checkout on macOS or Linux (including WSL). The installed CLI has no runtime package dependencies. Source checks use pinned TypeScript and Node type definitions; install these contributor tools with `npm ci --ignore-scripts --no-audit --no-fund`. Development scripts and tests are separate from the installed CLI; user setup is covered in the [README](../README.md).

## Local checks

```sh
npm run check
npm test
npm run test:package
```

The test suite uses local mocks and fake credentials. It covers Jev and Ollama routing, bounded prompt extraction, confidence and timeout fallback, token checks, model continuity, authentication forwarding, streaming, cancellation, status state, savings, and launcher behavior. Tests that start HTTP services require loopback binding. Local tests make no paid provider calls or model downloads.

Auto-mode regressions exercise Sonnet → Opus → Opus tool continuation → Sonnet in one conversation, with and without gateway prompt IDs. They retain signed thinking, native context edits, mid-conversation system messages, safety-review settings, and streamed verdicts, including denied actions. Separate capability tests keep unknown review contracts and incompatible model features from being routed.

Package validation checks the distributable and installed command rather than relying on the source checkout's paths. Review the [release procedure](releasing.md) before distributing a tarball.

## Request lifecycle and model continuity

The gateway validates the request containers it consumes, evaluates the current task, applies continuity and capacity rules, then checks the proposed target against the shared model catalog. Unknown provider extensions remain intact; when their compatibility with another model is unknown, the source model is retained with a routing reason. Token-count requests apply the same compatibility rules before sending a request.

`src/model-catalog.mjs` contains exact model IDs, capability facts, source links, and a review date. A family name inside a custom alias does not establish capabilities. `src/model-request.mjs` contains explicit thinking adaptations; neither layer strips signed history or permission-review settings. Model updates should change the catalog, include a dated authoritative source, and add a request fixture demonstrating the restriction or new supported switch.

The evaluation cache and active execution state have separate lifetimes. `src/turn-state.mjs` keeps active human tasks and pending tools beyond the evaluator cache TTL. Retired tasks expire, aliases and records are bounded, and exhausting active-state capacity is reported rather than silently evicting another active task. State is process-local; after a gateway restart, missing continuity is reported as unknown until a successful response establishes it again.

The gateway stages a selected model under its request ID. `src/response-observer.mjs` observes serving models, provider fallback boundaries, closed tool calls, usage, and terminal response metadata without altering bytes. A serving-model observation alone is not a successful execution. Only clean completion evidence followed by successful HTTP forwarding commits the continuation model. Cancelled, failed, ambiguous, and superseded attempts cannot overwrite known state. New human tasks remain eligible for upward or downward switching, including Sonnet/Opus in Auto mode.

Direct `Router.route()` embedders that omit `requestId` retain selected, unconfirmed continuity for compatibility. Embedders that execute inference should supply a unique request ID and call `router.complete(id, evidence)` after successful delivery, or `router.complete(id)` on failure. The HTTP gateway owns that lifecycle automatically.

## Evaluation acceptance

Evaluation reports distinguish evaluator availability, rubric agreement, routing policy, tier coverage, transport, and independently checked task completion. An unmeasured gate is explicitly marked unmeasured. Normal runs cannot pass solely on classifier fallback or cached predictions; simulated-outage runs explicitly require fallback. Compatible routing requires all three selected tiers, while Auto requires Sonnet and Opus. Constrained fixtures declare expected guard overrides.

The general and Ollama evaluation scripts accept `--min-agreement` and `--max-under-route-rate`. Their defaults require complete expected-label agreement and no under-routing. Set any alternative thresholds **before** evaluating a candidate, retain the fixture checksum with the report, and keep tuning cases separate from held-out cases. Rubric labels are judgments about synthetic tasks; these reports do not prove end-user task quality or subscription savings. The expanded corpus includes multilingual tasks, short difficult follow-ups, ordinary work in long background context, and task text containing tier-selection instructions. No prompt-policy adjustment should be justified by rerunning and relabeling the held-out set.

## Run from source

```sh
cp .env.example .env
# Set your Jev key, or select an installed local Ollama evaluator.
# Choose subscription or API-key authentication.
node --env-file=.env bin/autorouter.mjs doctor
node --env-file=.env bin/autorouter.mjs claude
```

The explicit Node flag loads `.env`; the CLI itself does not auto-load project files. Environment values override the user config. Keep keys out of source control and command arguments.

With AutoRouter 0.3.2 or newer, launch an already configured Ollama evaluator without its runtime deadline using:

```sh
AUTOROUTER_OLLAMA_TIMEOUT_MS=0 claude-autorouter claude
```

Persist the setting with `claude-autorouter setup --evaluator ollama --ollama-model tev1:4b --ollama-timeout-ms 0 --force` for that installed model. The setup flag overrides the timeout environment value; later launch-time environment values still override saved configuration. Startup priming keeps its separate 60-second limit, cancellation remains active, and normal errors still use fallback.

## Local routing regression

The source-only harness below is opt-in and is not included in the npm package. It sends synthetic Claude-shaped requests through the real router and an installed local evaluator, checking task extraction, classifier choices, selected Claude tiers, and new human turns. It makes no Anthropic or Jev calls, downloads no models, and writes no user configuration.

```sh
node scripts/test-ollama-routing.mjs --model tev1:0.8b
node scripts/test-ollama-routing.mjs --model tev1:4b
node scripts/test-ollama-routing.mjs --model nimble:9b-q4_K_M
```

Start Ollama 0.35+ and install the selected model first. The harness refuses to run while another model is resident. It never explicitly unloads the selected model; its keep-alive setting controls residency. It warms once, then uses the production deadline for each uncached case: 1,500 ms for Tev1 0.8B/custom tags, 15,000 ms for official Tev1 4B tags, and 30,000 ms for official Nimble tags. Environment settings or `--timeout-ms N` can override the deadline; `--timeout-ms 0` disables the runtime timer while retaining cancellation and the separate warmup limit. This harness does not load saved user configuration. Use `--output artifacts/local-routing.json` to save a metadata report.

```sh
node scripts/test-ollama-routing.mjs --model tev1:4b --timeout-ms 0
```

The command fails on a wrong classification, fallback, unexpected guard override, or missing tier coverage. A Sonnet result with `source: ollama` and `classified_tier: sonnet` is a valid prediction; `source: fallback` and `classifier_error: timeout` means classification did not complete. Passing establishes these synthetic cases only. Warmup and metadata-only `doctor` checks do not establish speed or accuracy on real tasks.

Version 0.3.2 excludes Claude's executor system instructions from local classifier input before excerpt budgeting and retains task/history excerpts. Jev is unchanged. Version 0.3.1 included the executor background locally and defaulted every local model to 1,500 ms; see the [upgrade notes](reference.md#migrating-an-older-ollama-config). Historical benchmark results must remain labeled with their original excerpt policy and explicit deadlines.

## Live integration tests

The Claude integration tests below make real Claude calls and invoke the configured evaluator, consuming Claude usage and, with Jev, TypeSafe usage. They use temporary synthetic fixtures and disable unrelated customizations and MCP servers. With Ollama, start the local service and install the chosen model first; the harness does not install or download it.

```sh
npm run test:live
npm run test:live -- --case coding
npm run test:live -- --case example_haiku,example_sonnet,example_opus
npm run test:live -- --case large_context
node scripts/live-validation.mjs --help
```

The harness checks response models, HTTP status, answers, tool use, continuation behavior, and independent tests for the coding fixture. The opt-in `large_context` case deliberately exceeds 200K input tokens. Reports contain metadata rather than request bodies or credentials; fixtures are removed afterward. Save any local reports outside the package allowlist.

For a classifier-only rubric evaluation:

```sh
npm run eval
```

The bundled evaluation makes 19 classifier calls and no Claude generations. The evaluation uses the configured evaluator, which defaults to a local Ollama model and needs an installed one; set `AUTOROUTER_EVALUATOR=jev` to evaluate with Jev, which incurs TypeSafe usage and sends redacted excerpts to TypeSafe. It reports agreement with the starting rubric, fallback count, and p50/p95 routing latency. Edit `test/fixtures/routing.json` to represent the tasks you want to measure. Rubric agreement alone does not establish answer quality or net savings; compare completed tasks against fixed-model baselines.

For local evaluator measurements, use Ollama 0.35+ and a model compatible with `/v1/systemone`. Distinguish cold model loading from warmed classification, and record the model tag, hardware, Ollama version, context size, prompt length, and resident memory. The launcher primes the classifier with a synthetic task before opening the UI, with a separate deadline of up to 60 seconds. Runtime and benchmark share the 3,000-character/3,000-UTF-8-byte state limit, so include non-ASCII cases and excerpts that fill the budget. Also measure the first request after keep-alive expiration: its reload can hit the normal deadline even when warm requests pass. Repeat on realistic prompt distributions instead of selecting a model from a single easy request. Disk download size is not resident RAM. Keep model downloads opt-in and respect each model's license.

The dedicated Ollama benchmark uses synthetic tuning/held-out fixtures and reports cold latency separately from repeated warm requests:

```sh
npm run eval:ollama -- --models nimble:9b-q4_K_M --split heldout --rounds 3 --stress-rounds 8
npm run eval:ollama -- --models tev1:0.8b,tev1:4b-q4_K_M --split heldout --rounds 1 --stress-rounds 8
node scripts/evaluate-ollama.mjs --help
```

Install each selected model first and use an idle Ollama instance with no resident models. The benchmark loads one candidate at a time and unloads it afterward. It does not download models or contact Claude or Jev. It reports classification errors and under/over-routing as well as latency; fixture labels are subjective rubric judgments, not measurements of completed task quality.

See the [local evaluator measurements](ollama-evaluation.md) for the hardware, results, and limits. The older Qwen chat adapter and its compact/quality/auto presets have been removed. Every local candidate now uses native choice scoring; context allocation comes from the model/server configuration. Native entropy confidence is not calibrated accuracy and is not used as Jev's confidence threshold. A successful launcher/Enterprise integration request establishes connectivity and model routing, not classifier accuracy or parity with Jev.

## Startup context diagnostics

The source-only probe launches Claude in a chosen repository and reports request sizes, tool counts, selected feature flags, and whether the entered prompt reaches the classifier excerpt. By default, a local stub answers and no request context goes to Jev or Anthropic:

```sh
node scripts/context-probe.mjs --cwd /path/to/project --tool-search unset
node scripts/context-probe.mjs --cwd /path/to/project --tool-search true
node scripts/context-probe.mjs --cwd /path/to/project --tool-search true --example haiku
```

Configured MCP servers still connect for discovery, and ordinary Claude startup customizations can run. The stub emits no tool calls. `--example` also accepts `sonnet` and `opus`.

On macOS, `--interactive` uses `/usr/bin/script` and requires a terminal on stdin. This measures the interactive tool set rather than print mode's tool set. The probe records terminal byte counts, not terminal contents, then stops after its response. Claude may save its normal transcript in interactive mode because its no-session-persistence option is limited to print mode.

`--classify` invokes the configured evaluator while Claude responses remain stubbed. With the default Jev backend, use it only when the selected repository's excerpts may be sent to TypeSafe; Ollama sends them to the local service. `--live` additionally sends the complete startup request to Anthropic, using `tool_choice: none` to prohibit model tool execution. Load configuration explicitly when using these modes:

```sh
node --env-file=.env scripts/context-probe.mjs --cwd /path/to/synthetic-fixture --example haiku --classify
```

Private repository or connected-tool context may be present even when the typed prompt is harmless. Keep private-payload investigations local unless external processing is authorized. For shareable live regressions, prefer the isolated synthetic fixtures above. The probe report itself persists only metadata.

## Static contracts and style

`npm run check` checks JavaScript syntax, TypeScript/JSDoc contracts for configuration, classifier results, final routing decisions and normalized telemetry, and literal event producers in transport code. `src/contracts.mjs` is the shared development-time type contract; normalizers remain the runtime privacy boundary. Negative fixtures in `test/static-contracts.mts` and `test/static-checks.test.mjs` prove misspelled fields, invalid enums, payload fields and timing strings are rejected before execution. Provider request extensions remain opaque and are validated only where the router consumes them.

Use two-space indentation, LF endings, one final newline, semicolons and single quotes for ordinary strings. Compact pure helpers are allowed when readable; do not reformat unrelated code. The style check rejects trailing whitespace, tab indentation, `var`, and coercing comparisons except deliberate null/undefined checks. TypeScript is a contributor dependency only; public packages keep zero runtime dependencies. CI installs the pinned lockfile before checks and never runs provider inference automatically.

## Versioned protocol evidence

`test/fixtures/claude-protocol-v1.json` is a versioned, newly authored synthetic corpus reviewed against the Messages API, streaming, deferred-tool and fallback contracts. `test/protocol-fixtures.test.mjs` sends it through the real gateway, router and response observer with fake evaluator/upstream services. It covers Auto floors and switches, thinking adaptation and preservation, custom deferred tool references, conservative built-in server-tool history, compaction, scoped goal feedback, parallel agents, model fallback/tool ownership, usage and truncated responses.

Historical Claude Code 2.1.284/2.1.285 report versions, dates and hashes are separate metadata. The corpus does not copy captured prompts, invent provider signatures or certify a live client. Current-source real-provider canaries remain opt-in and unmeasured unless a separate report records them.

## Performance regression measurements

[Router measurements](router-performance.md) and [status-storage measurements](status-performance.md) record the pre-change baseline, repeated candidate runs, hardware/background load and baseline-derived gates. Source-only harnesses use synthetic inputs and providers; no credentials, prompts from user sessions or downloads are involved.

```sh
node --expose-gc scripts/benchmark-router.mjs baseline /tmp/router-comparison.json
node --expose-gc scripts/benchmark-router.mjs candidate /tmp/router-comparison.json --check
node scripts/benchmark-status.mjs --label local-check --check
```

Identical concurrent classifier inputs share one bounded evaluation. Each request applies its own continuity, capacity and compatibility checks. Cancelling one waiter preserves other waiters; cancelling all releases the shared evaluation. Cache identity retains the complete request, requested model floor, evaluator configuration and rubric hash. New tasks and sequential pinned continuations still evaluate; prior-pin classification reuse was deliberately not enabled. A 64 KiB response limit applies to both evaluators, and local model metadata is limited to 1 MiB. Disabling the Ollama timer does not disable cancellation or these byte limits.

Status writes use one asynchronous writer and a coalesced latest snapshot. Embedders await `state.ready` before using its path, `flush()` when they need persisted evidence, and `close()` before cleanup. Initial storage failures or a one-second readiness timeout disable the optional display. Accepted in-flight writes finish before directory removal, so shutdown cannot recreate files.

The router/storage measurements cover one 16 GiB M4 with synthetic providers. Actual Tev1 4B and Nimble 9B measurements on that Mac and a 64 GiB M2 Ultra are recorded separately in [the hardware comparison](hardware-comparison.md). Both candidates missed the unchanged strict quality gate on both hosts. Model digests and runtime conditions differ, so the cross-host results do not isolate RAM's effect. Use the [transfer bundle instructions](hardware-benchmark.md) to reproduce the workload, recording background load and observed residency. Do not infer model performance from router timings.
