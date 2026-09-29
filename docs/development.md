# Development and validation

Use Node.js 22+ from a source checkout on macOS or Linux (including WSL). The project has no runtime package dependencies. Development scripts and tests are separate from the installed CLI; user setup is covered in the [README](../README.md).

## Local checks

```sh
npm run check
npm test
npm run test:package
```

The test suite uses local mocks and fake credentials. It covers routing, bounded prompt extraction, confidence and timeout fallback, token checks, model continuity, authentication forwarding, streaming, cancellation, status state, savings, and launcher behavior. Tests that start HTTP services require loopback binding. Local tests make no paid provider calls.

Package validation checks the distributable and installed command rather than relying on the source checkout's paths. Review the [release procedure](releasing.md) before distributing a tarball.

## Run from source

```sh
cp .env.example .env
# Set your Jev key; choose subscription or API-key authentication.
node --env-file=.env bin/autorouter.mjs doctor
node --env-file=.env bin/autorouter.mjs claude
```

The explicit Node flag loads `.env`; the CLI itself does not auto-load project files. Environment values override the user config. Keep keys out of source control and command arguments.

## Live integration tests

These tests make real Jev and Claude calls, consuming provider usage. They use temporary synthetic fixtures and disable unrelated customizations and MCP servers.

```sh
npm run test:live
npm run test:live -- --case coding
npm run test:live -- --case example_haiku,example_sonnet,example_opus
npm run test:live -- --case large_context
node scripts/live-validation.mjs --help
```

The harness checks response models, HTTP status, answers, tool use, continuation behavior, and independent tests for the coding fixture. The opt-in `large_context` case deliberately exceeds 200K input tokens. Reports contain metadata rather than request bodies or credentials; fixtures are removed afterward. Save any local reports outside the package allowlist.

For a Jev-only rubric evaluation:

```sh
npm run eval
```

The bundled evaluation makes 12 paid Jev calls and no Claude generations. It reports agreement with the starting rubric, fallback count, and p50/p95 routing latency. Edit `test/fixtures/routing.json` to represent the tasks you want to measure. Rubric agreement alone does not establish answer quality or net savings; compare completed tasks against fixed-model baselines.

## Startup context diagnostics

The source-only probe launches Claude in a chosen repository and reports request sizes, tool counts, selected feature flags, and whether the entered prompt reaches the classifier excerpt. By default, a local stub answers and no request context goes to Jev or Anthropic:

```sh
node scripts/context-probe.mjs --cwd /path/to/project --tool-search unset
node scripts/context-probe.mjs --cwd /path/to/project --tool-search true
node scripts/context-probe.mjs --cwd /path/to/project --tool-search true --example haiku
```

Configured MCP servers still connect for discovery, and ordinary Claude startup customizations can run. The stub emits no tool calls. `--example` also accepts `sonnet` and `opus`.

On macOS, `--interactive` uses `/usr/bin/script` and requires a terminal on stdin. This measures the interactive tool set rather than print mode's tool set. The probe records terminal byte counts, not terminal contents, then stops after its response. Claude may save its normal transcript in interactive mode because its no-session-persistence option is limited to print mode.

Use `--classify` only when the selected repository's excerpts may be sent to TypeSafe. It makes real Jev calls while Claude responses remain stubbed. `--live` additionally sends the complete startup request to Anthropic, using `tool_choice: none` to prohibit model tool execution. Load credentials explicitly when using these modes:

```sh
node --env-file=.env scripts/context-probe.mjs --cwd /path/to/synthetic-fixture --example haiku --classify
```

Private repository or connected-tool context may be present even when the typed prompt is harmless. Keep private-payload investigations local unless external processing is authorized. For shareable live regressions, prefer the isolated synthetic fixtures above. The probe report itself persists only metadata.
