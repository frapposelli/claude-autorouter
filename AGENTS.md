# Agent contribution guide

AutoRouter is an independent local gateway for Claude Code. It evaluates tasks and routes eligible requests to compatible Haiku, Sonnet or Opus models. Jev is the default evaluator; local Ollama evaluators are experimental. Both evaluator integrations use `/v1/systemone`.

These instructions apply throughout this repository. Start with [CONTRIBUTING.md](CONTRIBUTING.md), then read the relevant code and tests. Use [README.md](README.md) for user-facing behavior and [docs/reference.md](docs/reference.md) for configuration and routing policy. Read detailed guides as needed rather than loading every document into context.

## Environment and repository map

Use Node.js 22+ on macOS or Linux, including WSL. Runtime code is JavaScript ESM (`.mjs`) with JSDoc contracts; the CLI has no runtime package dependencies. TypeScript and Node type definitions are pinned development tools. Follow existing style and keep developer tooling out of runtime dependencies.

| Area | Where to start |
| --- | --- |
| CLI, launch and help | `bin/autorouter.mjs`, `src/onboarding.mjs`, `src/cli-help.mjs` |
| Configuration and saved settings | `src/config.mjs`, `src/user-config.mjs`, `src/config-command.mjs` |
| Gateway, authentication and streaming | `src/server.mjs`, `src/auth.mjs`, `src/response-observer.mjs` |
| Evaluation and routing | `src/router.mjs`, `src/ollama-evaluator.mjs`, `src/prompt-state.mjs`, `src/auto-routing.mjs` |
| Compatibility and task continuity | `src/model-catalog.mjs`, `src/model-request.mjs`, `src/turn-state.mjs`, `src/token-counter.mjs` |
| Contracts, logs, status and savings | `src/contracts.mjs`, `src/telemetry-event.mjs`, `src/session-log.mjs`, `src/status-state.mjs`, `src/savings.mjs` |
| Regression coverage and packaging | `test/`, `test/fixtures/`, `scripts/package-smoke.mjs`, `scripts/release-pack.mjs` |

## Local validation

Install contributor tooling from the lockfile:

```sh
npm ci --ignore-scripts --no-audit --no-fund
```

For a focused iteration, run the relevant test file, for example:

```sh
node --test test/router.test.mjs
```

Before handing off code or packaging changes, run the same checks as CI:

```sh
npm run check
npm test
npm run test:package
```

`check` verifies syntax, TypeScript/JSDoc contracts, event producers and style. Tests use synthetic credentials and local mocks; HTTP tests need loopback binding. `test:package` verifies the actual archive and installed CLI, including launcher lifecycle and cleanup. There is no separate build or lint command. For documentation-only changes, validate links and whitespace; do not add tests that merely mirror prose. All four Node/OS CI jobs remain required for merging.

Live tests, evaluator calls, hardware benchmarks and model downloads are opt-in. Do not run them as ordinary contribution checks: `npm run test:live` consumes Claude/evaluator usage, and `npm run eval` uses paid Jev calls by default. `scripts/context-probe.mjs` can execute configured MCP servers and startup customizations even when inference is stubbed. Follow [development and validation](docs/development.md) for task-requested live work, record its conditions, and label mock and real-provider evidence separately.

## Preserve routing and protocol invariants

- Keep Claude in charge of authentication, account eligibility and permission decisions. Do not weaken local gateway authentication, replace permission verdicts, or work around provider account restrictions.
- Preserve provider response bytes, signed history, permission-review settings and unfamiliar extensions. Unknown model IDs or extensions are not evidence of compatibility; use the shared catalog and conservative guards. Token counting and inference must apply the same compatibility policy.
- Keep active human-task/tool state separate from disposable evaluator caches. New human tasks remain eligible to switch models; Auto mode permits compatible Sonnet/Opus switching. A selected or observed model is not a confirmed successful execution: confirm continuity only after clean completion and successful forwarding. Failed, cancelled, ambiguous or superseded requests must not overwrite confirmed state.
- Keep input/response/storage bounds, deadlines, cancellation and cleanup intact. Disabling an evaluator timer must not disable cancellation or byte limits. Shared classifier calls must retain independent waiter cancellation.

The [request lifecycle](docs/development.md#request-lifecycle-and-model-continuity) documents these boundaries. Add meaningful regressions for behavior changes, using synthetic fixtures rather than captured user sessions.

## Contracts, privacy and model changes

- Update `src/contracts.mjs` with configuration, decision or telemetry changes; align event producers, normalizer allowlists, readers and privacy tests. Keep older supported session-log schemas readable.
- Logging stays disabled by default. Optional prompt-mode history contains task excerpts; metadata mode must omit them. Use synthetic payloads and fake credentials in fixtures. Do not commit real credentials, private prompts, transcripts or captured user request/response bodies; keep payloads and credentials out of routine diagnostics. Use [SECURITY.md](SECURITY.md) for private vulnerability reports.
- For model or pricing changes, follow the [contributor checklist](CONTRIBUTING.md#model-and-pricing-updates): exact provider IDs, dated authoritative sources, capability fixtures and explicit pricing versions. Do not guess capabilities from family names or reprice historical logs without recorded provenance.
- For evaluator or performance changes, follow the [evaluation guidance](CONTRIBUTING.md#evaluation-and-performance). Declare thresholds before measurement, retain held-out fixtures and distinguish transport, classification and task-quality results. Do not tune against a relabeled held-out set or present mock timing as local-model performance.

## Delivering changes

Keep diffs focused, preserve unrelated work, and update affected help and documentation. Open a PR against `main` using the existing [contribution workflow](CONTRIBUTING.md#submit-a-change); keep required checks and repository protections intact. State what changed, why, which checks ran, and any material limits or untested paths.

Version bumps, release tags, npm publication and user-configuration edits are separate work; include them only when the task calls for them. Follow [docs/releasing.md](docs/releasing.md) for releases. The package has an explicit file allowlist: keep these agent instruction files source-only, and verify archive contents if packaging changes.
