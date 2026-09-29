# Development and validation

Use Node.js 22+ from a source checkout on macOS or Linux (including WSL). The project has no runtime package dependencies. Development scripts and tests are separate from the installed CLI; user setup is covered in the [README](../README.md).

## Local checks

```sh
npm run check
npm test
npm run test:package
```

The test suite uses local mocks and fake credentials. It covers Jev and Ollama routing, bounded prompt extraction, confidence and timeout fallback, token checks, model continuity, authentication forwarding, streaming, cancellation, status state, savings, and launcher behavior. Tests that start HTTP services require loopback binding. Local tests make no paid provider calls or model downloads.

Package validation checks the distributable and installed command rather than relying on the source checkout's paths. Review the [release procedure](releasing.md) before distributing a tarball.

## Run from source

```sh
cp .env.example .env
# Set your Jev key, or select an installed local Ollama evaluator.
# Choose subscription or API-key authentication.
node --env-file=.env bin/autorouter.mjs doctor
node --env-file=.env bin/autorouter.mjs claude
```

The explicit Node flag loads `.env`; the CLI itself does not auto-load project files. Environment values override the user config. Keep keys out of source control and command arguments.

## Local routing regression (unreleased)

The source-only harness below is opt-in and is not included in npm 0.3.1. It sends synthetic Claude-shaped requests through the real router and an installed local evaluator, checking task extraction, classifier choices, selected Claude tiers, and new human turns. It makes no Anthropic or Jev calls, downloads no models, and writes no user configuration.

```sh
node scripts/test-ollama-routing.mjs --model tev1:0.8b
node scripts/test-ollama-routing.mjs --model tev1:4b
node scripts/test-ollama-routing.mjs --model nimble:9b-q4_K_M
```

Start Ollama 0.35+ and install the selected model first. The harness refuses to run while another model is resident. It never explicitly unloads the selected model; its keep-alive setting controls residency. It warms once, then uses the production deadline for each uncached case: 1,500 ms for Tev1 0.8B/custom tags, 15,000 ms for official Tev1 4B tags, and 30,000 ms for official Nimble tags. Environment settings or `--timeout-ms N` can override the deadline; this harness does not load saved user configuration. Use `--output artifacts/local-routing.json` to save a metadata report.

The command fails on a wrong classification, fallback, unexpected guard override, or missing tier coverage. A Sonnet result with `source: ollama` and `classified_tier: sonnet` is a valid prediction; `source: fallback` and `classifier_error: timeout` means classification did not complete. Passing establishes these synthetic cases only. Warmup and metadata-only `doctor` checks do not establish speed or accuracy on real tasks.

The unreleased local path excludes Claude's executor system instructions before excerpt budgeting and retains task/history excerpts. Jev is unchanged. npm 0.3.1 still includes the executor background locally and defaults every local model to 1,500 ms; see the [published-version timeout workarounds](reference.md#classification-and-fallback). Historical benchmark results must remain labeled with their original excerpt policy and explicit deadlines.

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

The bundled evaluation makes 12 classifier calls and no Claude generations. Jev is the default and incurs TypeSafe usage; set `AUTOROUTER_EVALUATOR=ollama` to evaluate an installed local model. It reports agreement with the starting rubric, fallback count, and p50/p95 routing latency. Edit `test/fixtures/routing.json` to represent the tasks you want to measure. Rubric agreement alone does not establish answer quality or net savings; compare completed tasks against fixed-model baselines.

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
