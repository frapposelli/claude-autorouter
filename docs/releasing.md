# CI and npm releases

The package is `claude-autorouter`, licensed under [Apache-2.0](../LICENSE). Version `0.2.0` was published manually to [npm](https://www.npmjs.com/package/claude-autorouter) on September 29, 2026. npm trusted publishing is configured for this repository's `publish.yml`, including direct publication permission. Subsequent releases use [version tags](#3-release-subsequent-versions-by-tag). Preparing a tarball or merging a pull request does not publish it.

Version `0.3.1` replaces the old Ollama chat evaluator and Qwen presets with the native `/v1/systemone` endpoint on Ollama 0.35+. It supports Nimble, Tev1, and other compatible local models through `--ollama-model`; Jev remains the default remote evaluator. Existing local users should rerun setup with a supported model, as described in the [reference](reference.md#ollama-evaluator). The [evaluation report](ollama-evaluation.md) records local model latency, accuracy, and timeout limitations.

Version `0.3.2` fixes local timeout fallbacks with model-specific deadlines, removes Claude executor instructions from local evaluator excerpts, and keeps fallback causes visible in compact status lines. It also adds `AUTOROUTER_OLLAMA_TIMEOUT_MS=0` and `setup --ollama-timeout-ms 0` to disable the runtime evaluation deadline while preserving caller cancellation and the separate startup warmup limit. Existing explicit timeout settings still override the defaults; Jev is unchanged.

Version `0.3.3` fixes HTTP 400 errors when a compatible request with disabled thinking is routed to Sonnet 5.5. Inference and token counting translate that setting to `between_tools`, or adaptive thinking when effort settings require it. Model defaults are unchanged; select Sonnet 5.5 with `AUTOROUTER_SONNET_MODEL=claude-sonnet-5-5`.

Version `0.3.4` preserves the selected model across Stop-hook feedback for the same prompt, including `/goal` commands that omit the gateway prompt-ID header. Recognized goal feedback remains conversation context rather than replacing the human task in evaluator excerpts. Goal-checker verdicts remain unchanged; external authorization blockers can still cause Claude's own goal loop to repeat. See [goal troubleshooting](reference.md#troubleshooting).

Version `0.3.5` adds opt-in saved configuration for Claude's native `CLAUDE_CODE_STOP_HOOK_BLOCK_CAP`, with `setup --stop-hook-block-cap N`, validation, and `doctor` reporting. A value of `2` permits two consecutive Stop-hook continuations without tool use and ends the turn on the third blocking verdict, leaving an unmet goal set. This affects all Stop/SubagentStop hooks, and tool activity resets the counter. Defaults and completion verdicts are unchanged; `0` disables the guard. See [shorter Stop-hook loops](reference.md#shorter-stop-hook-loops-opt-in).

Version `0.3.6` fixes Auto permission-mode launches with an Auto-compatible Sonnet/Opus profile while preserving Claude's permission classifiers and server safety-review requests. It also adds optional per-session JSONL decision logs containing a bounded human prompt excerpt, selected model, and routing latency. Logging is disabled by default; set `AUTOROUTER_SESSION_LOG_DIR` or use `setup --session-log-dir DIR` to enable it. See [Auto permission mode](reference.md#auto-permission-mode) and [session decision logs](reference.md#session-decision-logs).

Version `0.3.7` enables automatic Sonnet/Opus switching for compatible Auto-mode execution requests, including requests carrying the known server safety-review contract. The Auto profile defaults to Sonnet 5.5 and Opus 5.5, floors Haiku decisions to Sonnet, and retains the selected model through tool and goal continuations. Shared native context edits, mid-conversation system messages, and signed thinking history no longer pin new human tasks. Permission-classifier requests and safety verdicts remain unchanged; unknown contracts and incompatible model features still preserve a compatible model. Explicit model overrides remain in effect. See [Auto permission mode](reference.md#auto-permission-mode).

Version `0.4.0` completes the routing, configuration, history and performance improvement plan. Shared compatibility checks and durable task state preserve valid request features and confirmed tool/goal continuity across evaluator cache expiry, provider fallback and concurrent requests. New `config show/set/unset`, `sessions list/show` and `doctor --evaluate-local` commands support focused configuration edits, private metadata-only history and explicit local diagnostics. `setup --force` now merges saved settings; use `--replace` for deliberate replacement. Optional logging remains disabled by default; new schema-2 decision/outcome records separate selected and observed models, while the reader still accepts schema-1 files. Consumers parsing JSONL directly should account for both event kinds and the new schema. Identical concurrent evaluations are coalesced with independent cancellation, responses are bounded, and status persistence is asynchronous. Releases retain the tested archive and verify public npm availability and installation after submission. Actual 16 GiB/64 GiB Ollama results retain failed quality gates and comparison limits; Jev remains the default. See the [configuration/history reference](reference.md) and [hardware comparison](hardware-comparison.md).

Version `0.5.0` makes local Ollama the default evaluator and TypeSafe Jev an explicit option (`setup --evaluator jev`), so evaluator excerpts stay on the machine unless the user opts in. **Breaking for environment-only launches** that relied on the implicit Jev default; configurations created by `setup` record their evaluator and are unchanged. Evaluator excerpts and opt-in session-log prompt excerpts are now redacted for recognizable credentials and personal identifiers (pattern-based, not exhaustive). On macOS, new `setup` runs keep saved keys in the login Keychain by default; existing plaintext configurations are not moved implicitly, and `doctor` prints the `config set AUTOROUTER_SECRET_STORE keychain` command to move them. See [credential storage](reference.md#credential-storage) and the [data flow](reference.md#data-flow-and-authentication).

Version `0.5.1` hardens defaults and adds an optional organization policy. **Behavior change:** enabling `AUTOROUTER_SESSION_LOG_DIR` now records metadata only; set `AUTOROUTER_SESSION_LOG_MODE=prompts` to keep prompt excerpts. Saved configurations that already record a mode are unchanged. A root-owned policy file can restrict the evaluator and authentication mode and lock the log mode and service URLs; see [organization policy](reference.md#organization-policy). The next launch removes status directories left by a hard-killed launcher, an Ollama `localhost` endpoint now connects to `127.0.0.1`, an oversized request body closes its connection after the 413 response, and `.env.example` selects the local evaluator to match the default.

Version `0.5.2` widens secret redaction and closes a Keychain command-injection path. Redaction now also covers URL credentials with an empty user or a password containing `@`, passwords containing `;` or spaces in quotes, `*_KEY`, `*_PASS` and `*_AUTH` names, cookies, command-line credentials (`curl -u`, `--password`, `--api-key`, `sshpass -p`, `mysql -p`), about fifteen more provider token formats, international phone numbers, checksum-validated Italian tax codes and US social security numbers. **Behavior change:** evaluator excerpts and prompt-mode logs contain more `[REDACTED:...]` markers than before, and recognizable personal identifiers now appear as `[REDACTED:phone]` or `[REDACTED:national_id]`. A configuration path containing control or line-separator characters is now rejected, and Keychain item names are validated before they reach the `security` tool. Contributor documentation no longer names Jev as the default evaluator. See the [data flow](reference.md#data-flow-and-authentication).

The GitHub repository became public on October 6, 2026, after preparation PR #1 merged. That launch created no release tag and published no new npm version. npm publication remains a separate release operation. Its tarball includes runtime source, README, configuration example, license, and shipped documentation; model weights, user configuration, credentials, transcripts, session logs, local artifacts, and test fixtures are excluded. Review each release archive, especially when the package allowlist changes. The public Git repository also exposes history, development scripts and tests; the npm archive allowlist does not govern that material.

## What runs automatically

| Workflow | Trigger | Behavior |
| --- | --- | --- |
| [ci.yml](https://github.com/frapposelli/claude-autorouter/blob/main/.github/workflows/ci.yml) | Pull requests, pushes to `main`, manual runs, and calls from the release workflow | Syntax checks, tests, and package smoke tests on Ubuntu/macOS with Node 22/24 |
| [publish.yml](https://github.com/frapposelli/claude-autorouter/blob/main/.github/workflows/publish.yml) | Tag push; manual verification-only dispatch | Test and submit one canonical archive; independently verify registry availability and installation. Manual dispatch never publishes. |

A release tag must exactly equal `v` plus the version in `package.json`, and its commit must be reachable from `origin/main`. Package name and repository metadata must match `claude-autorouter` and `frapposelli/claude-autorouter`. Stable versions use npm's `latest` tag; prereleases such as `0.3.1-beta.1` use `next`.

The release workflow packs its candidate once and smoke-tests that exact `.tgz`. It retains the archive, SHA-256 checksum, and commit-based release notes as Actions artifacts. The publishing job downloads the canonical artifact by immutable ID, checks every packaged file against the release checkout, and checks fresh npm metadata before submission. A new stable version must be greater than the current stable `latest`. An already-visible identical version skips publication; a different archive under that version is an error. Registry/network errors never count as proof that a version is unused.

Only the publishing job has `id-token: write`; there is no `NPM_TOKEN` or required GitHub environment. It runs on GitHub-hosted Ubuntu with Node 24 and npm 11.19.1. The separate verifier has read-only permissions and never changes npm distribution tags. The workflow serializes its publishers, but independent/manual publishers must coordinate: npm does not provide an atomic compare-and-swap for the `latest` tag. A newer `latest` observed during verification is reported as superseding this release and is never moved backward.

Verification polls uncached version metadata and the package document, checks the downloaded tarball against the tested archive, and installs the exact version from the public registry into a temporary prefix with an empty npm cache/config. It compares the installed file set and bytes with the canonical archive before invoking that executable’s `--version` and `--help` from an unrelated directory, with lifecycle scripts disabled and no evaluator credentials. Only a successful public install and matching artifact produce `verified`.

The installed CLI has no runtime dependencies. Contributor tooling uses pinned development dependencies and a committed `package-lock.json`; CI installs them with `npm ci --ignore-scripts --no-audit --no-fund` before checks. Live Claude/Jev calls, Ollama downloads, and private repository probes are not CI checks.

## 1. Publish the first version interactively

This bootstrap was completed for `claude-autorouter@0.2.0`. Do not repeat it for this package; continue with [trusted publishing](#2-authorize-this-workflow-on-npm). The instructions below are retained as the bootstrap procedure for a new package name.

Merge the release workflows and package metadata to `main`, push to GitHub, and ensure GitHub Actions is enabled for the repository. Its Actions policy must permit the pinned official GitHub actions and the reusable CI workflow in this repository. Use a clean checkout of that commit, with Node 24 and npm 11.19.1 to match the publisher. No release tag is needed for this bootstrap. First check the registry and account:

```sh
npm ping --registry https://registry.npmjs.org/
npm view claude-autorouter name version --registry https://registry.npmjs.org/
npm whoami --registry https://registry.npmjs.org/
```

For a new package name, verify availability and ownership before continuing. A network or authentication failure is not evidence that a name is available. If another owner has claimed the name, choose an available name and update package metadata, release validation, documentation, and trust settings together.

If `whoami` reports `ENEEDAUTH`, sign in interactively and complete npm's browser/2FA prompts:

```sh
npm login --registry https://registry.npmjs.org/
npm whoami --registry https://registry.npmjs.org/
```

Use your own npm account with publishing permission, and keep credentials out of the repository and workflow secrets. See [npm login](https://docs.npmjs.com/cli/v11/commands/npm-login/) and [publishing a public package](https://docs.npmjs.com/creating-and-publishing-unscoped-public-packages/) for account requirements.

For the initial `0.2.0` release, run:

```sh
npm run check
npm test
npm run release:pack
npm run test:package -- --archive ./dist/claude-autorouter-0.2.0.tgz
tar -tzf ./dist/claude-autorouter-0.2.0.tgz
```

`release:pack` checks the allowed files and writes `dist/claude-autorouter-0.2.0.tgz` and its `.sha256` file. The `--archive` smoke test installs and exercises those bytes without repacking. Review the listed contents and retain both files. If anything changes, rebuild and repeat the exact-archive smoke test.

Publish that reviewed candidate, completing npm's interactive authentication challenge when requested:

```sh
npm publish ./dist/claude-autorouter-0.2.0.tgz --ignore-scripts --access public --tag latest --registry https://registry.npmjs.org/ --provenance=false
npm view claude-autorouter@0.2.0 version dist.integrity --registry https://registry.npmjs.org/
```

On a separate machine or disposable environment, verify the registry installation:

```sh
npm install -g claude-autorouter@0.2.0
claude-autorouter --version
claude-autorouter --help
```

Then run `setup`, `doctor`, and a launch from outside the source checkout as appropriate for that machine. `doctor` is local-only; a live prompt separately verifies provider access. Keep the README's installation instructions aligned with the verified registry release.

Do not push `v0.2.0` to test automation after this bootstrap. Keep the original bootstrap archive; a current verifier only accepts releases matching its archive and repository validation rules. npm name/version pairs cannot be reused, including after unpublishing. See the [npm publish reference](https://docs.npmjs.com/cli/v11/commands/npm-publish/).

## 2. Authorize this workflow on npm

Trusted publishing requires the package to exist first, which is why the initial version is published manually. The account configuring trust needs package write access and 2FA. See [npm trust prerequisites](https://docs.npmjs.com/cli/v11/commands/npm-trust/#prerequisites).

On npmjs.com, open the `claude-autorouter` package's **Settings → Trusted Publisher**, select **GitHub Actions**, and enter:

| Setting | Value |
| --- | --- |
| Organization or user | `frapposelli` |
| Repository | `claude-autorouter` |
| Workflow filename | `publish.yml` |
| Environment name | Leave blank |
| Allowed actions | Enable direct `npm publish` |

Use the filename only, not `.github/workflows/publish.yml`. No GitHub environment or npm token secret needs to be created. The owner, repository, and workflow must match exactly. New trust configurations default to permitting staged publication; **enable direct `npm publish`** for this workflow. See [npm trusted publishers](https://docs.npmjs.com/trusted-publishers/) and [staged publishing](https://docs.npmjs.com/staged-publishing/).

The publishing workflow selects provenance from repository visibility. With the source now public, the workflow requests provenance on future publication; it would disable provenance for a private source repository. OIDC authentication works independently of provenance. The package preparation job's dry run always disables provenance because it has no OIDC publishing permission. Before the first public-source release, review repository metadata and npm trust settings, then verify the resulting provenance statement. The public launch itself created no attestation, and changing visibility does not add attestations to historical releases. See [npm provenance requirements](https://docs.npmjs.com/generating-provenance-statements/).

After a successful trusted release, npm recommends the optional **Publishing access → Require two-factor authentication and disallow tokens** setting. It does not disable OIDC publishing. See [restricting token access](https://docs.npmjs.com/trusted-publishers/#recommended-restrict-token-access-when-using-trusted-publishers).

## 3. Release subsequent versions by tag

Use the next unused version. The following commands use `0.4.0` as an example, not as a claim that it is currently available:

```sh
git switch main
git pull --ff-only origin main
npm version 0.4.0 --no-git-tag-version
npm run check
npm test
npm run release:pack
npm run test:package -- --archive ./dist/claude-autorouter-0.4.0.tgz
git diff --check
```

Review the archive, version-specific documentation, and release notes. Commit all intended changes and get that commit onto `main` through the repository’s normal review process. Wait for CI to pass, then tag the exact release commit:

```sh
git switch main
git pull --ff-only origin main
git tag -a v0.4.0 -m "Release 0.4.0"
git push origin v0.4.0
```

The tag must match `package.json`. For a prerelease, use matching values such as `0.4.0-beta.1` / `v0.4.0-beta.1`; publication uses `next`, leaving `latest` unchanged. Release stable versions in increasing order, and wait for verification or investigate a pending submission before starting another stable release. The preflight blocks a new stable candidate that is not newer than the registry’s `latest`; queue order alone does not establish version order.

For local checks after a tag exists, `node scripts/release-check.mjs source v0.4.0` validates the clean checkout, tag, metadata, and main ancestry. `node scripts/release-check.mjs archive v0.4.0` validates the candidate checksum and contents. `dist/` must contain only that candidate’s `.tgz` and `.sha256`, so retain older artifacts elsewhere.

## 4. Inspect the release state and retain evidence

Open the tag’s run under [GitHub Actions](https://github.com/frapposelli/claude-autorouter/actions). Submission success is not proof that users can install the package. The verification job’s summary and retained JSON report distinguish:

| State | Meaning | Next step |
| --- | --- | --- |
| `preflight_ready` | The new candidate passed metadata and version-order checks; submission has not occurred | The initial workflow attempt may submit it |
| `submitted` | npm accepted the command, or an identical immutable version was already visible | Wait for independent verification |
| `validating_unavailable` | Metadata, tarball, distribution tag, or installation is still unavailable, or the registry is failing | Keep the original archive and rerun verification |
| `verified` | Exact archive integrity, distribution-tag state, and isolated public installation passed | Use the recorded upgrade command |
| `failed` | An input, provenance, integrity, version-order, or executable check failed | Investigate the report before changing anything |

The verifier polls for up to 15 minutes with backoff, then reports pending verification with a successful command exit. **A green workflow can therefore mean pending, not verified; read the recorded state.** An npm processing delay does not establish publication failure or justify a duplicate release. HTTP/network failures are reported separately from a missing version or tarball.

The workflow retains these artifacts for 90 days, subject to repository retention policy:

- `npm-package-<run-id>-<attempt>`: the canonical tested archive and SHA-256 file.
- `release-notes-<run-id>-<attempt>`: notes derived from the tagged source’s commits and archive identity.
- `release-submission-<run-id>-<attempt>`: preflight and, when accepted, submission reports.
- `release-verification-<run-id>-<attempt>`: the canonical archive, checksum, available notes, and verification report.

Download and retain the archive/checksum, notes, and report before Actions artifacts expire; attaching them to a GitHub Release is suitable for long-term retention. Artifact expiration is not a reason to repack a supposedly identical candidate for verification.

After the report says `verified`, use its exact version:

```sh
npm install -g claude-autorouter@0.4.0
claude-autorouter --version
claude-autorouter --help
```

The verifier runs the equivalent exact-version registry install in isolation. If `latest` has since advanced, the report explicitly marks this release as superseded; it does not restore an older tag. An unqualified `npm install -g claude-autorouter` follows the registry’s current `latest` instead.

## 5. Rerun verification without publishing

Use the Actions **Run workflow** control for `publish.yml`, or the command below. Provide the release tag and the numeric run/artifact IDs from the original tag-triggered run:

```sh
gh workflow run publish.yml --ref main \
  -f tag=v0.4.0 \
  -f run_id=ORIGINAL_RUN_ID \
  -f artifact_id=CANONICAL_NPM_PACKAGE_ARTIFACT_ID
```

This path validates that the artifact belongs to this repository’s original tag-triggered publishing workflow and matches the tag commit on `main`. It downloads those immutable bytes, checks their checksum and package metadata, and verifies npm availability. It cannot publish or alter npm tags and does not need npm OIDC permissions. Old runs may lack retained release notes; that does not prevent archive verification.

You can also download the original archive and its checksum and run the verifier independently from a current source checkout:

```sh
node scripts/release-verify.mjs verify v0.4.0 \
  --archive /path/to/claude-autorouter-0.4.0.tgz \
  --report artifacts/release-verification-0.4.0.json \
  --timeout-ms 900000
```

Both files must retain their original names, and the archive must satisfy the public-package validation rules. This command performs only registry reads and a temporary isolated install; it neither publishes nor changes your installed CLI, npm login, or AutoRouter configuration. Exit `0` includes pending verification, so automation must inspect the report’s `state`; exit `1` means verification failed. A mismatch is never accepted as an already-published identical release.

## Recovering a release

- **Checks/archive validation failed:** nothing was submitted by that failed path. Fix the cause, repeat validation, and follow the normal release process. Never move a published tag.
- **Registry preflight failed:** an HTTP/authentication/network failure is not evidence that the version is unused. Restore visibility before submitting.
- **npm rejected OIDC:** check the trusted-publisher fields, direct-publish permission, hosted runner, and OIDC permission. A job rerun deliberately does not resubmit a still-invisible version because an interrupted command may already have been accepted. Establish what happened before planning a new submission.
- **Submission timed out, the run was interrupted, or npm is processing it:** retain the original archive and use verification-only dispatch. A retry with a matching visible version skips `npm publish`; a retry with an invisible version only verifies it.
- **Existing version has different bytes:** stop. Never overwrite, unpublish, or replace the canonical artifact to reuse that version. Corrections require a new version.
- **Verification remains pending:** do not call it a failed publication. Rerun the verifier later with the original artifact, inspect npm’s public status if the registry is failing, and retain the evidence for support if validation remains stuck.

The workflow never repairs distribution tags automatically. npm’s [publish semantics](https://docs.npmjs.com/cli/commands/npm-publish/) make published name/version pairs immutable; [distribution tags](https://docs.npmjs.com/adding-dist-tags-to-packages/) are mutable references, so verification observes them without overwriting a newer release.
