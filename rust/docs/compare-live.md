# Paired live validation

This development command runs the existing isolated live-validation harness
against the frozen Node gateway and native gateway. It preserves protocol v2,
the six v2 fixtures, exact configured models, original rubric and relative
acceptance gates. It requires the existing Claude login and hosted Jev settings;
it does not modify permissions, authenticate another account or download models.

Read `rust/parity/live-orchestration-v1.json`. Build a matching xtask helper
separately from the ordinary shipping CLI, keep all 111 frozen reference files,
and retain source/build hashes. A source commit argument is a declaration;
capture independent source/commit/build evidence before approving results.

The benchmark transfer includes the declared build sources, protocols and
synthetic fixtures. It does not include every repository Source-audit capture
script or the TypeScript development tools those scripts need. Reproduce those
audits from a contributor checkout; the self-contained native doctor tests are
included in the transfer's Rust test sources.

Prepare makes no inference or authentication request. It checks bounded local
help/version commands, key presence, public configuration and exact identities:

```sh
./target/release/xtask compare-live prepare \
  --node /absolute/path/to/node22.14.0 \
  --claude /absolute/path/to/existing/claude \
  --cargo /absolute/path/to/existing/cargo \
  --source-commit COMMIT_SHA40 \
  --conditions normal-use-builds-paused-auth-unchanged \
  --output results/live-declaration-1
```

Supply credentials through the existing environment or the explicit xtask
`--env-file` prefix. Secret values stay in memory/inherited child environments;
they are never passed as CLI arguments or included in projected configuration.
The command does not inspect or fingerprint Claude's account store. The operator
must keep the same authenticated account and credential settings throughout.

Run is an explicit paid-usage opt-in. Pause builds/tests, retain normal-use
machine conditions, and let every child complete before starting another:

```sh
./target/release/xtask compare-live run \
  --declaration results/live-declaration-1 \
  --conditions normal-use-builds-paused-auth-unchanged \
  --output results/live-run-1
```

There are 20 harness invocations and 120 synthetic cases: five alternating pairs
for compatible/dontAsk and five for Auto/actual permission mode auto. Provider
calls exceed 120 because of continuations, tools and auxiliary requests. No fixed
API-call or cost budget is claimed. The command omits `--case` so the existing
full-profile default retains nonempty Haiku/Sonnet/Opus or Sonnet/Opus coverage.
Selected v2 cases currently have an unrelated required-tier derivation bug; they
cannot qualify this full comparison.

Each child has a 900s deadline, 5s TERM-to-KILL grace and 2 MiB combined retained
stdout/stderr quota. Only admitted original sanitized case/final reports are
written; raw stderr and malformed/unexpected stdout are discarded. Known
inherited credentials are checked against report strings in memory; this is
an additional guard, not a detector for arbitrary unrelated secrets.

Every pair requires the original relative gates without rounding or sample
exclusions. A private synthetic prompt index binds each route's expected label
to the fixed fixture; retries may repeat an index. Unknown or missing task
mapping cannot qualify the fixture comparison. Invalid/private/worker reports
stop future admissions; complete validated quality-failure reports may continue.
Absolute baseline/candidate model quality failures remain visible,
even when relative gates pass. Tokens, cache use, evaluator counts and latencies
are descriptive separate evidence. Outputs are create-only; preserve failures
and interruptions and use a new directory after correcting the cause.

Normal cleanup records awaited gateway/router close, an actual connection-refused
observation after listener close, existing fixture removal and joined outer
helper. Native GatewayHandle suppresses worker JoinError, so its internal outcome
is unavailable. Nested Claude/reference commands own separate groups; outer
SIGKILL cannot prove termination of every nested or escaped process. Abnormal
or missing reports keep cleanup unverified and cannot qualify. No daemon,
arbitrary descendant reaping or private account-store preservation is claimed.

These finite live results do not approve general model quality, controlled
hardware/performance, shipping provenance, supported platforms or release.
