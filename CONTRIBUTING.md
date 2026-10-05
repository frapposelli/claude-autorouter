# Contributing to AutoRouter

Use Node.js 22+ and macOS or Linux. Install pinned development tools, then run the local checks:

```sh
npm ci --ignore-scripts --no-audit --no-fund
npm run check
npm test
npm run test:package
```

Tests use synthetic local services and credentials. They require loopback binding, but make no paid provider calls, downloads or user-config changes. The package check installs and exercises the exact distributable archive. Opt-in provider/model canaries are described in [development and validation](docs/development.md).

## Changing behavior

Follow the [request lifecycle](docs/development.md#request-lifecycle-and-model-continuity). Keep authentication and permission decisions owned by Claude. Preserve provider bytes, signed history and unfamiliar extensions. Routing must check compatibility in every profile; new human tasks remain eligible to switch models. Active task state is separate from disposable classification caches, and only clean, successfully forwarded completion evidence can establish confirmed continuation state.

Update `src/contracts.mjs` alongside configuration, decision or telemetry changes. Keep the normalizer allowlist and privacy tests aligned. Logged selections are not successful outcomes, logging stays opt-in, and metadata mode must omit prompts. [Static/style checks](docs/development.md#static-contracts-and-style) run in CI; do not add runtime dependencies for developer tooling.

## Model and pricing updates

1. Identify the exact provider model ID; a family keyword or custom alias is not capability evidence.
2. Update `src/model-catalog.mjs` with the authoritative source and review date. Verify context/output limits, thinking, tool choice, native tool features and Auto eligibility.
3. Add valid-source compatibility fixtures for every affected profile and a regression for the new restriction or permitted switch. Keep unknown models/extensions conservative.
4. Update thinking adaptation only where documented; token counting and inference must apply the same compatibility policy.
5. If rates change, review `src/savings.mjs`, bump its pricing version/date and source, and test cache TTL/modifier/unknown-model coverage. Never silently price old logs using an unrecorded new table.
6. Run all three local checks and exact-package fixtures. Report separately any explicitly invoked real-provider observations and their limits.

## Evaluation and performance

Declare label agreement, acceptable tiers and under-routing thresholds before testing a candidate. Preserve fixture checksums and held-out cases; transport, evaluator availability, policy and task quality are separate gates. Profile coverage requires all three tiers for compatible routing and Sonnet/Opus for Auto.

Capture a baseline before changing overhead. Repeat the same workload and hardware with the [router/storage harnesses](docs/development.md#performance-regression-measurements), retaining call counts, latency distributions, memory and background load. Numerical timing gates are local and opt-in; CI checks deterministic cancellation/resource behavior. Real local-model benchmarks need representative memory sizes and observed cold/warm conditions.

## Releasing

Use the [release procedure](docs/releasing.md) and retain the tested immutable archive. Acceptance by npm and public availability are separate states. Verify registry integrity and an isolated install before calling a release verified. A pending submission is investigated or verified again, rather than blindly republished. New CLI interfaces and event-schema changes should be reviewed together as a minor release; correctness-only fixes can be independent patches.
