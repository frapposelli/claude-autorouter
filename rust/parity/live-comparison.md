# Matched live task-quality runs

`cargo xtask live-validation` defaults to the native gateway. Its explicit
`--engine node-reference` mode imports the hash-verified, frozen 0.5.2 gateway
and router through a temporary source-only adapter. Both modes use the same
version 2 fixtures, prompts, Claude command, disposable working directories,
allowed tools, Rust repair source and independent Cargo verifier. Neither mode
uses the historical JavaScript repair task as if it were the version 2 task.

These commands make real Claude subscription and configured evaluator calls.
They are opt-in; they are not ordinary tests, and no live comparison is implied
by the synthetic reference-adapter test. Node 22+ is needed only when selecting
the temporary reference engine. The native product never starts it.

From `rust/`, with an explicitly prepared environment, a matched pair is:

```sh
cargo xtask --env-file ../.env live-validation --engine node-reference
cargo xtask --env-file ../.env live-validation --engine native
```

Keep the client version, profile, evaluator/model IDs, environment, selected
cases, thinking settings, timeout, tool permissions and machine conditions
fixed. Alternate engine order across repeated trials and retain every result.
Match `fixture_version`, `fixture_sha256` and all three repair-file hashes before
comparing the transport, classification, policy and task-quality gates. Reports
identify the engine; reference reports additionally identify the frozen source
commit, adapter hash and actual Node version. A selected-case run retains its
restricted coverage label. Live availability and task quality remain separate
from deterministic gateway equivalence and local transport timings.

For actual Auto permission-mode coverage, set the routing profile to `auto` and
explicitly pass `--permission-mode auto` in both matched runs. The default
`dontAsk` permission mode does not establish that coverage. Auto mode eligibility
and safety decisions remain owned by Claude; the canary does not bypass them.
The [version 2 comparison protocol](live-comparison-protocol-v2.json) declares
the distinct compatible/Auto modes and remaining qualification requirements.

The explicit reference-adapter regression uses only synthetic loopback HTTP,
fake credentials and the shared coding prompt; it launches no Claude executable:

```sh
cargo test -p xtask frozen_gateway_routes_identical_v2_fixture_and_reports_only_metadata -- --ignored
```

The adapter sends only readiness, routing metadata and ordinary gateway event
fields to the Rust report collector. Provider responses still pass directly to
the client; fixture answers and credential values are not report fields.

## Isolated real Keychain comparison

On macOS, the temporary reference driver can compare both migration directions
against the real `security` adapter without touching existing credentials:

```sh
node parity/check-keychain-migration.mjs --real-keychain \
  ../artifacts/rust-rewrite/reference ./target/release/claude-autorouter \
  ../artifacts/rust-rewrite/keychain-comparison-new
```

Run from `rust/` after freezing the reference and building the candidate. The
output directory must be new. This explicit opt-in creates a disposable private
configuration, requires all three derived credential accounts to be absent,
uses synthetic values, verifies migration and shared account identity, then
deletes only its own items. It honors system policy and Keychain consent; an
unavailable or rejected operation remains a failed result. No provider calls
are made. The [declared protocol](keychain-comparison-protocol-v1.json) and
[host preflight summary](measurements/resume-keychain-preflight-summary.json)
describe the bounds, results and remaining platform limitations.
